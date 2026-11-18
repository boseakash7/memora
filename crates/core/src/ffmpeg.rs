//! Runs the `ffmpeg` / `ffprobe` executables. They are looked up next to the app's
//! executable first (where the installer puts them), then on `PATH`.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use image::DynamicImage;
use serde_json::Value;

use crate::{Error, Result};

const TIMEOUT: Duration = Duration::from_secs(60);
/// Storyboards decode every keyframe of the video, which takes longer for long 4K videos.
const STORYBOARD_TIMEOUT: Duration = Duration::from_secs(300);

fn program(name: &str) -> PathBuf {
    let exe = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    if let Ok(current) = std::env::current_exe()
        && let Some(dir) = current.parent()
        && dir.join(&exe).is_file()
    {
        return dir.join(exe);
    }
    PathBuf::from(exe)
}

pub(crate) fn command(name: &str) -> Command {
    let mut cmd = Command::new(program(name));
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Don't flash a console window from the GUI app.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        let priority = if BACKGROUND.get() { BELOW_NORMAL_PRIORITY_CLASS } else { 0 };
        cmd.creation_flags(CREATE_NO_WINDOW | priority);
    }
    cmd
}

thread_local! {
    static BACKGROUND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mark the calling thread as background work (indexing, recognition): it runs at
/// below-normal priority, and so do the FFmpeg processes it starts. Windows then serves the
/// app's window, video playback and other programs first, so the PC stays responsive while
/// a big library is indexed.
pub fn mark_background_thread() {
    BACKGROUND.set(true);
    #[cfg(windows)]
    {
        use std::ffi::c_void;
        unsafe extern "system" {
            fn GetCurrentThread() -> *mut c_void;
            fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
        }
        const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;
        // SAFETY: GetCurrentThread returns a pseudo-handle for this thread that needs no
        // closing; SetThreadPriority only changes its scheduling priority.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }
    }
}

/// Run to completion and return stdout. Kills the process if it hangs.
pub(crate) fn run(cmd: Command) -> Result<Vec<u8>> {
    run_for(cmd, TIMEOUT)
}

/// [`run`] with its own time limit.
fn run_for(mut cmd: Command, timeout: Duration) -> Result<Vec<u8>> {
    let mut child = cmd.spawn().map_err(|e| {
        Error::media(format!("cannot start {:?} (is FFmpeg installed?): {e}", cmd.get_program()))
    })?;

    // Drain both pipes on threads so a chatty process can't block on a full pipe.
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::media("FFmpeg took too long and was stopped"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if !status.success() {
        let msg = String::from_utf8_lossy(&stderr);
        let last = msg.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("unknown error");
        return Err(Error::media(format!("FFmpeg failed: {}", last.trim())));
    }
    Ok(stdout)
}

/// `ffprobe` output (format + streams) as JSON.
pub fn probe(path: &Path) -> Result<Value> {
    let mut cmd = command("ffprobe");
    cmd.args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams", "-i"])
        .arg(path);
    let out = run(cmd)?;
    Ok(serde_json::from_slice(&out)?)
}

fn scale_filter(max: u32) -> String {
    format!("scale=w='min(iw,{max})':h='min(ih,{max})':force_original_aspect_ratio=decrease")
}

fn decode_bmp(cmd: &mut Command) -> Result<DynamicImage> {
    decode_bmp_for(cmd, TIMEOUT)
}

fn decode_bmp_for(cmd: &mut Command, timeout: Duration) -> Result<DynamicImage> {
    cmd.args(["-frames:v", "1", "-f", "image2pipe", "-vcodec", "bmp", "-"]);
    let out = run_for(std::mem::replace(cmd, Command::new("")), timeout)?;
    if out.is_empty() {
        return Err(Error::media("FFmpeg produced no image"));
    }
    Ok(image::load_from_memory_with_format(&out, image::ImageFormat::Bmp)?)
}

/// Decode the video frame at `seek_secs`, scaled down to fit in `max` x `max`.
/// FFmpeg applies the video's rotation; HDR video is tone mapped to normal colours.
pub fn decode_frame(path: &Path, seek_secs: Option<f64>, max: u32) -> Result<DynamicImage> {
    let mut cmd = command("ffmpeg");
    cmd.args(["-v", "error", "-nostdin"]);
    if let Some(s) = seek_secs.filter(|s| *s > 0.0) {
        cmd.args(["-ss", &format!("{s:.3}")]);
    }
    cmd.arg("-i").arg(path);
    let mut filter = scale_filter(max);
    if let Some(hdr) = hdr_of(path) {
        filter = format!("{filter},{}", tone_map_filter(hdr));
    }
    cmd.args(["-vf", &filter]);
    decode_bmp(&mut cmd)
}

/// One picture holding `cols` x `rows` small frames of a video, left to right and top to
/// bottom, one every `interval` seconds from the start, each `tile` pixels (rotation
/// applied). Unused tiles at the end are black. For the viewer's seek-bar preview.
///
/// Only keyframes are decoded (phones write one every second or two), so a tile shows the
/// keyframe nearest its time. That is many times faster than decoding every frame: about
/// 0.7 s for an 81 s 1080p60 HEVC phone video.
pub fn storyboard(path: &Path, interval: f64, tile: (u32, u32), cols: u32, rows: u32) -> Result<DynamicImage> {
    let mut cmd = command("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-hwaccel", "auto", "-skip_frame", "nokey", "-i"]).arg(path);
    let mut filter = format!("fps={:.6},scale={}:{}", 1.0 / interval.max(0.001), tile.0, tile.1);
    if let Some(hdr) = hdr_of(path) {
        filter = format!("{filter},{}", tone_map_filter(hdr));
    }
    filter = format!("{filter},tile={cols}x{rows}");
    cmd.args(["-an", "-sn", "-vf", &filter]);
    decode_bmp_for(&mut cmd, STORYBOARD_TIMEOUT)
}

// ---------- HDR video ----------

/// How an HDR video encodes brightness. Shown as it is, HDR looks grey and washed out on
/// normal screens and TVs, so converted copies and thumbnails are tone mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hdr {
    /// PQ (SMPTE ST 2084): HDR10, HDR10+, most Android phones.
    Pq,
    /// Hybrid log-gamma: iPhones (also under Dolby Vision 8.4), some cameras.
    Hlg,
}

/// The HDR kind of a video's first video stream, if any. Probed once per file and session.
pub fn hdr_of(path: &Path) -> Option<Hdr> {
    let modified = path.metadata().and_then(|m| m.modified()).ok();
    let cache = hdr_cache();
    if let Some((when, hdr)) = cache.lock().unwrap().get(path)
        && *when == modified
    {
        return *hdr;
    }
    let mut cmd = command("ffprobe");
    cmd.args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=color_transfer", "-of", "json", "-i"])
        .arg(path);
    let hdr = run(cmd).ok().and_then(|out| serde_json::from_slice::<Value>(&out).ok()).and_then(|v| hdr_of_probe(&v));
    cache.lock().unwrap().insert(path.to_path_buf(), (modified, hdr));
    hdr
}

/// Note the HDR kind from a full probe made anyway (the metadata stage), so making the
/// thumbnail later needs no second probe.
pub(crate) fn remember_hdr(path: &Path, probe: &Value) {
    let modified = path.metadata().and_then(|m| m.modified()).ok();
    hdr_cache().lock().unwrap().insert(path.to_path_buf(), (modified, hdr_of_probe(probe)));
}

type HdrCache = Mutex<HashMap<PathBuf, (Option<SystemTime>, Option<Hdr>)>>;

fn hdr_cache() -> &'static HdrCache {
    static CACHE: OnceLock<HdrCache> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The first real video stream's transfer (cover pictures don't count).
pub(crate) fn hdr_of_probe(probe: &Value) -> Option<Hdr> {
    let streams = probe["streams"].as_array()?;
    let stream = streams
        .iter()
        .find(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1)
        .or_else(|| streams.first())?;
    match stream["color_transfer"].as_str()? {
        "smpte2084" => Some(Hdr::Pq),
        "arib-std-b67" => Some(Hdr::Hlg),
        _ => None,
    }
}

/// FFmpeg filters that turn HDR (BT.2020) into 8-bit SDR (BT.709) 4:2:0.
///
/// HDR video puts paper white at about 203 nits (ITU-R BT.2408), so that is mapped to SDR
/// white; the Möbius curve keeps colours below it as they are and squeezes the brighter
/// highlights into the rest of the range.
pub fn tone_map_filter(hdr: Hdr) -> String {
    let transfer = match hdr {
        Hdr::Pq => "smpte2084",
        Hdr::Hlg => "arib-std-b67",
    };
    format!(
        "zscale=tin={transfer}:pin=bt2020:min=bt2020nc:t=linear:npl=203,format=gbrpf32le,\
         zscale=p=bt709,tonemap=tonemap=mobius:desat=0,zscale=t=bt709:m=bt709:r=tv,format=yuv420p"
    )
}

// ---------- HEIC / HEIF / AVIF ----------

/// Layout of a still image file (HEIC/HEIF/AVIF).
///
/// Phones store HEIC photos as a grid of small tiles (e.g. 48 tiles of 512x512 for a
/// 12 MP photo). FFmpeg 7.1 reports the grid but its command line does not stitch it,
/// so we build the stitching filter ourselves.
#[derive(Debug, Clone, PartialEq)]
pub struct Still {
    /// Size of the full picture, before rotation.
    pub width: u32,
    pub height: u32,
    /// Rotation from the file's display matrix (`irot`), degrees as FFmpeg reports them.
    pub rotation: Option<i64>,
    source: StillSource,
}

#[derive(Debug, Clone, PartialEq)]
enum StillSource {
    Stream(usize),
    Grid { tiles: Vec<(usize, u32, u32)>, x: u32, y: u32 },
}

impl Still {
    /// The layout read from the file's boxes (see `heif`), which saves opening the file in
    /// FFmpeg twice. None when the boxes don't settle it; then use [`probe_still`].
    pub fn from_heif(info: &crate::heif::Info) -> Option<Still> {
        let source = match info.layout.as_ref()? {
            crate::heif::Layout::Single { stream } => StillSource::Stream(*stream),
            crate::heif::Layout::Grid { tiles } => StillSource::Grid { tiles: tiles.clone(), x: 0, y: 0 },
        };
        Some(Still { width: info.width, height: info.height, rotation: None, source })
    }

    /// The layout without a display matrix, for comparing with [`Still::from_heif`].
    #[cfg(test)]
    fn same_layout(&self, other: &Still) -> bool {
        (self.width, self.height, &self.source) == (other.width, other.height, &other.source)
    }

    /// EXIF-style orientation (1-8) for the display-matrix rotation, if the file has one.
    pub fn orientation(&self) -> Option<u8> {
        match self.rotation?.rem_euclid(360) {
            90 => Some(8),  // 90° counter-clockwise
            180 => Some(3),
            270 => Some(6), // 90° clockwise
            _ => None,
        }
    }
}

pub fn probe_still(path: &Path) -> Result<Still> {
    let mut cmd = command("ffprobe");
    cmd.args(["-v", "error", "-print_format", "json", "-show_streams", "-show_stream_groups", "-i"])
        .arg(path);
    parse_still(&serde_json::from_slice(&run(cmd)?)?)
}

fn rotation_of(v: &Value) -> Option<i64> {
    v["side_data_list"].as_array()?.iter().find_map(|d| d["rotation"].as_f64()).map(|r| r.round() as i64)
}

fn parse_still(probe: &Value) -> Result<Still> {
    let streams = probe["streams"].as_array().cloned().unwrap_or_default();
    let u32_of = |v: &Value| v.as_u64().map(|n| n as u32);

    let grid = probe["stream_groups"].as_array().and_then(|groups| {
        groups.iter().find(|g| g["type"] == "Tile Grid" && g["disposition"]["default"] == 1)
            .or_else(|| groups.iter().find(|g| g["type"] == "Tile Grid"))
    });
    if let Some(g) = grid {
        let c = &g["components"][0];
        let tiles: Vec<(usize, u32, u32)> = c["subcomponents"]
            .as_array()
            .map(|subs| {
                subs.iter()
                    .filter_map(|s| {
                        Some((
                            s["stream_index"].as_u64()? as usize,
                            u32_of(&s["tile_horizontal_offset"])?,
                            u32_of(&s["tile_vertical_offset"])?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let (Some(width), Some(height), false) = (u32_of(&c["width"]), u32_of(&c["height"]), tiles.is_empty()) {
            let first = tiles[0].0;
            let rotation = rotation_of(c)
                .or_else(|| streams.iter().find(|s| s["index"].as_u64() == Some(first as u64)).and_then(rotation_of));
            return Ok(Still {
                width,
                height,
                rotation,
                source: StillSource::Grid {
                    tiles,
                    x: u32_of(&c["horizontal_offset"]).unwrap_or(0),
                    y: u32_of(&c["vertical_offset"]).unwrap_or(0),
                },
            });
        }
    }

    // A single image: the default stream, else the largest one (files also carry a small preview).
    let video: Vec<&Value> = streams.iter().filter(|s| s["codec_type"] == "video").collect();
    let area = |s: &&Value| s["width"].as_u64().unwrap_or(0) * s["height"].as_u64().unwrap_or(0);
    let main = video
        .iter()
        .find(|s| s["disposition"]["default"] == 1)
        .or_else(|| video.iter().max_by_key(|s| area(s)))
        .ok_or_else(|| Error::media("no image found in file"))?;
    Ok(Still {
        width: u32_of(&main["width"]).unwrap_or(0),
        height: u32_of(&main["height"]).unwrap_or(0),
        rotation: rotation_of(main),
        source: StillSource::Stream(main["index"].as_u64().unwrap_or(0) as usize),
    })
}

/// FFmpeg arguments that turn the still into one picture labelled `[out]`, fitting `max`.
fn still_filter(still: &Still, max: u32) -> String {
    let scale = scale_filter(max);
    match &still.source {
        StillSource::Stream(i) => format!("[0:{i}]{scale}[out]"),
        StillSource::Grid { tiles, x, y } => {
            let crop = format!("crop={}:{}:{x}:{y}", still.width, still.height);
            if tiles.len() == 1 {
                return format!("[0:{}]{crop},{scale}[out]", tiles[0].0);
            }
            let inputs: String = tiles.iter().map(|(i, _, _)| format!("[0:{i}]")).collect();
            let layout: Vec<String> = tiles.iter().map(|(_, tx, ty)| format!("{tx}_{ty}")).collect();
            format!("{inputs}xstack=inputs={}:layout={},{crop},{scale}[out]", tiles.len(), layout.join("|"))
        }
    }
}

/// Decode a HEIC/HEIF/AVIF to fit in `max` x `max`, without applying rotation
/// (callers rotate using [`Still::orientation`] or EXIF).
pub fn decode_still(path: &Path, still: &Still, max: u32) -> Result<DynamicImage> {
    let mut cmd = command("ffmpeg");
    // One thread per process: a tiled photo has ~48 streams and FFmpeg would start a thread
    // pool for each, which is slower than the decoding itself. We decode several files in
    // parallel instead (about 3x faster overall).
    cmd.args(["-v", "error", "-nostdin", "-threads", "1", "-filter_threads", "1", "-noautorotate", "-i"])
        .arg(path);
    cmd.args(["-filter_complex", &still_filter(still, max), "-map", "[out]"]);
    decode_bmp(&mut cmd)
}

/// True when `ffmpeg` and `ffprobe` can be started. Used by tests to skip video cases.
pub fn available() -> bool {
    ["ffmpeg", "ffprobe"].iter().all(|n| {
        let mut cmd = command(n);
        cmd.arg("-version");
        run(cmd).is_ok()
    })
}

#[cfg(test)]
pub(crate) fn make_test_video(path: &Path, extra: &[&str]) {
    let mut cmd = command("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=10:duration=2"]);
    cmd.args(extra);
    cmd.args(["-pix_fmt", "yuv420p"]).arg(path);
    run(cmd).expect("ffmpeg makes a test video");
}

/// A 10-bit HEVC clip of one colour (sRGB 200, 60, 40), encoded as HDR with SDR white
/// at 203 nits, the way phones and cameras record it.
#[cfg(test)]
pub(crate) fn make_hdr_video(path: &Path, hdr: Hdr) {
    let t = match hdr {
        Hdr::Pq => "smpte2084",
        Hdr::Hlg => "arib-std-b67",
    };
    let mut cmd = command("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=0xC83C28:s=320x240:d=1,format=yuv420p", "-vf"]);
    cmd.arg(format!(
        "zscale=tin=bt709:pin=bt709:min=bt709:rin=tv:t={t}:npl=203:p=bt2020:m=bt2020nc:r=tv,format=yuv420p10le"
    ));
    cmd.args(["-c:v", "libx265", "-x265-params", "log-level=none", "-color_primaries", "bt2020"]);
    cmd.args(["-color_trc", t, "-colorspace", "bt2020nc"]).arg(path);
    run(cmd).expect("ffmpeg makes an HDR test video");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Shape of `ffprobe -show_streams -show_stream_groups` for a phone HEIC (trimmed to 2x2 tiles).
    fn grid_probe(rotation: Option<i64>) -> Value {
        let side = rotation.map(|r| json!([{"side_data_type": "Display Matrix", "rotation": r}])).unwrap_or(json!([]));
        json!({
            "streams": [
                {"index": 0, "codec_type": "video", "width": 512, "height": 512, "side_data_list": side},
                {"index": 1, "codec_type": "video", "width": 512, "height": 512},
                {"index": 2, "codec_type": "video", "width": 512, "height": 512},
                {"index": 3, "codec_type": "video", "width": 512, "height": 512},
                {"index": 4, "codec_type": "video", "width": 320, "height": 240, "disposition": {"default": 0}}
            ],
            "stream_groups": [{
                "index": 0, "type": "Tile Grid", "disposition": {"default": 1},
                "components": [{
                    "nb_tiles": 4, "width": 1000, "height": 750, "horizontal_offset": 0, "vertical_offset": 0,
                    "subcomponents": [
                        {"stream_index": 0, "tile_horizontal_offset": 0, "tile_vertical_offset": 0},
                        {"stream_index": 1, "tile_horizontal_offset": 512, "tile_vertical_offset": 0},
                        {"stream_index": 2, "tile_horizontal_offset": 0, "tile_vertical_offset": 512},
                        {"stream_index": 3, "tile_horizontal_offset": 512, "tile_vertical_offset": 512}
                    ]
                }]
            }]
        })
    }

    #[test]
    fn tiled_heic_is_stitched_and_cropped() {
        let still = parse_still(&grid_probe(None)).unwrap();
        assert_eq!((still.width, still.height, still.orientation()), (1000, 750, None));
        assert_eq!(
            still_filter(&still, 2560),
            "[0:0][0:1][0:2][0:3]xstack=inputs=4:layout=0_0|512_0|0_512|512_512,crop=1000:750:0:0,\
             scale=w='min(iw,2560)':h='min(ih,2560)':force_original_aspect_ratio=decrease[out]"
        );
    }

    #[test]
    fn rotation_from_display_matrix() {
        assert_eq!(parse_still(&grid_probe(Some(-90))).unwrap().orientation(), Some(6));
        assert_eq!(parse_still(&grid_probe(Some(90))).unwrap().orientation(), Some(8));
        assert_eq!(parse_still(&grid_probe(Some(180))).unwrap().orientation(), Some(3));
    }

    #[test]
    fn single_image_uses_largest_stream() {
        let probe = json!({"streams": [
            {"index": 0, "codec_type": "video", "width": 160, "height": 120},
            {"index": 1, "codec_type": "video", "width": 1920, "height": 1080}
        ]});
        let still = parse_still(&probe).unwrap();
        assert_eq!((still.width, still.height), (1920, 1080));
        assert!(still_filter(&still, 400).starts_with("[0:1]scale="));
    }

    #[test]
    fn hdr_video_thumbnails_keep_their_colours() {
        if !available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        for hdr in [Hdr::Pq, Hdr::Hlg] {
            let path = dir.path().join(format!("{hdr:?}.mp4"));
            make_hdr_video(&path, hdr);
            assert_eq!(hdr_of(&path), Some(hdr));
            let px = decode_frame(&path, None, 64).unwrap().to_rgb8().get_pixel(10, 10).0;
            // Without tone mapping this comes out as a washed-out (108, 73, 48).
            let close = |a: u8, b: u8| a.abs_diff(b) <= 20;
            assert!(close(px[0], 200) && close(px[1], 60) && close(px[2], 40), "{hdr:?}: {px:?}");
        }
        let sdr = dir.path().join("sdr.mp4");
        make_test_video(&sdr, &[]);
        assert_eq!(hdr_of(&sdr), None);
    }

    /// Checks that the layout read from HEIF boxes is exactly what FFmpeg reports, for every
    /// HEIC/HEIF/AVIF in a folder (files are only read):
    /// `PHOTOS_TEST_DIR=folder cargo test -p photos-core --release -- --ignored --nocapture heif_layout_matches_ffmpeg`
    #[test]
    #[ignore]
    fn heif_layout_matches_ffmpeg() {
        use rayon::prelude::*;
        let dir = std::env::var("PHOTOS_TEST_DIR").expect("set PHOTOS_TEST_DIR");
        let files: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| crate::media::file_type(p).is_some_and(|(_, f)| f == crate::media::Format::FfmpegStill))
            .collect();
        let results: Vec<(PathBuf, &str)> = files
            .par_iter()
            .map(|p| {
                let ours = crate::heif::read(p).and_then(|i| Still::from_heif(&i));
                let verdict = match (ours, probe_still(p)) {
                    (None, _) => "not read from boxes (FFmpeg is asked)",
                    (Some(_), Err(_)) => "FFmpeg cannot read it",
                    (Some(a), Ok(b)) if a.same_layout(&b) => "same",
                    (Some(_), Ok(_)) => "DIFFERENT",
                };
                (p.clone(), verdict)
            })
            .collect();
        let mut counts = std::collections::BTreeMap::new();
        for (p, verdict) in &results {
            *counts.entry(*verdict).or_insert(0) += 1;
            if *verdict != "same" {
                println!("{verdict}: {}", p.display());
            }
        }
        println!("{counts:?}");
        assert!(!counts.contains_key("DIFFERENT"));
    }

    /// Decode a real HEIC: `PHOTOS_TEST_HEIC=path\to\photo.heic cargo test -- --ignored real_heic`
    #[test]
    #[ignore]
    fn real_heic() {
        let path = std::env::var("PHOTOS_TEST_HEIC").expect("set PHOTOS_TEST_HEIC");
        let still = probe_still(Path::new(&path)).unwrap();
        let img = decode_still(Path::new(&path), &still, 100_000).unwrap();
        assert_eq!((img.width(), img.height()), (still.width, still.height));
        println!("{still:?}");
    }
}
