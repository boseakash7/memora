//! On-the-fly video conversion for TVs and browsers that can't play the original
//! (HEVC in a browser, MKV/AVI files, odd audio...). FFmpeg writes H.264 + AAC at up to
//! 1080p to its stdout; the server streams that to the client as it arrives.
//!
//! The output has no known length, so seeking means starting FFmpeg again at a new time.

use std::process::Command;
use std::sync::OnceLock;

use crate::ffmpeg;
use crate::media::{Kind, Media};

/// Output container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// MPEG transport stream: what TVs play most reliably over DLNA.
    Ts,
    /// Fragmented MP4: what browsers' `<video>` can play while it downloads.
    Mp4,
}

impl Container {
    pub fn mime(self) -> &'static str {
        match self {
            Container::Ts => "video/mpeg",
            Container::Mp4 => "video/mp4",
        }
    }
}

/// An H.264 encoder FFmpeg can use on this PC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    Nvidia,
    Intel,
    Amd,
    Cpu,
}

impl Encoder {
    fn args(self) -> &'static [&'static str] {
        match self {
            Encoder::Nvidia => &[
                "-c:v", "h264_nvenc", "-preset", "p4", "-rc", "vbr", "-cq", "23", "-b:v", "0",
                "-maxrate", "16M", "-bufsize", "32M", "-profile:v", "high",
            ],
            Encoder::Intel => &["-c:v", "h264_qsv", "-preset", "faster", "-global_quality", "23", "-profile:v", "high"],
            Encoder::Amd => &["-c:v", "h264_amf", "-quality", "speed", "-rc", "cqp", "-qp_i", "22", "-qp_p", "24"],
            Encoder::Cpu => &["-c:v", "libx264", "-preset", "veryfast", "-crf", "22", "-profile:v", "high"],
        }
    }

    fn name(self) -> &'static str {
        self.args()[1]
    }
}

/// The fastest encoder that works here: hardware first (much faster for 4K sources and
/// leaves the CPU free), else x264. Found once by encoding a tiny test clip with each.
pub fn encoder() -> Encoder {
    static FOUND: OnceLock<Encoder> = OnceLock::new();
    *FOUND.get_or_init(|| {
        let found = [Encoder::Nvidia, Encoder::Intel, Encoder::Amd]
            .into_iter()
            .find(|e| works(*e))
            .unwrap_or(Encoder::Cpu);
        tracing::info!("video conversion uses {}", found.name());
        found
    })
}

fn works(e: Encoder) -> bool {
    let mut cmd = ffmpeg::command("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-f", "lavfi", "-i", "color=black:s=320x240:d=0.2", "-frames:v", "3"]);
    cmd.args(e.args());
    cmd.args(["-pix_fmt", "yuv420p", "-f", "null", "-"]);
    ffmpeg::run(cmd).is_ok()
}

/// Fits the picture in 1920x1080 (1080x1920 for portrait) without upscaling, with even
/// sizes for the encoder. FFmpeg applies rotation itself.
const SCALE_FILTER: &str = "scale=w='if(gte(iw,ih),min(iw,1920),min(iw,1080))':\
     h='if(gte(iw,ih),min(ih,1080),min(ih,1920))':\
     force_original_aspect_ratio=decrease:force_divisible_by=2";

/// Scaling, then 8-bit 4:2:0, which every TV decodes. HDR is tone mapped to SDR (after
/// scaling, which makes it much cheaper for 4K sources).
fn video_filter(hdr: Option<ffmpeg::Hdr>) -> String {
    match hdr {
        Some(hdr) => format!("{SCALE_FILTER},{}", ffmpeg::tone_map_filter(hdr)),
        None => format!("{SCALE_FILTER},format=yuv420p"),
    }
}

/// FFmpeg command that converts `media` from `start_secs` on and writes to stdout.
/// stdout and stderr are piped.
pub fn command(media: &Media, container: Container, start_secs: f64) -> Command {
    command_with(media, container, start_secs, encoder())
}

fn command_with(media: &Media, container: Container, start_secs: f64, encoder: Encoder) -> Command {
    let mut cmd = ffmpeg::command("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-hide_banner"]);
    // Decode on the graphics card when it can (falls back to the CPU by itself). Phone
    // videos are often 4K60 HEVC, which the CPU alone decodes slower than real time.
    cmd.args(["-hwaccel", "auto"]);
    if start_secs > 0.0 {
        // Before -i: jumps to the nearest keyframe quickly instead of decoding up to it.
        cmd.args(["-ss", &format!("{start_secs:.3}")]);
    }
    cmd.arg("-i").arg(&media.path);
    cmd.args(["-map", "0:v:0", "-map", "0:a:0?", "-sn", "-dn"]);
    cmd.args(["-vf", &video_filter(ffmpeg::hdr_of(&media.path))]);
    cmd.args(encoder.args());
    // A keyframe every 2 seconds keeps restarts (seeking) and stream starts quick.
    cmd.args(["-force_key_frames", "expr:gte(t,n_forced*2)"]);
    cmd.args(["-c:a", "aac", "-b:a", "192k", "-ac", "2", "-max_muxing_queue_size", "1024"]);
    match container {
        Container::Ts => cmd.args(["-f", "mpegts", "-muxdelay", "0"]),
        Container::Mp4 => cmd.args(["-f", "mp4", "-movflags", "frag_keyframe+empty_moov+default_base_moof"]),
    };
    cmd.arg("pipe:1");
    cmd
}

/// True when a TV is likely to play the original file: H.264 or HEVC in a container TVs
/// read, with sound they decode (or none). Unknown codecs count as not playable.
pub fn tv_plays_original(media: &Media) -> bool {
    let container_ok = matches!(media.mime.as_str(), "video/mp4" | "video/quicktime" | "video/x-matroska" | "video/mp2t");
    let video_ok = matches!(media.video_codec.as_deref(), Some("h264" | "hevc"));
    let audio_ok = matches!(media.audio_codec.as_deref(), Some("" | "aac" | "mp3" | "ac3" | "eac3"));
    media.kind == Kind::Video && container_ok && video_ok && audio_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn video(mime: &str, v: Option<&str>, a: Option<&str>) -> Media {
        let mut m = Media::for_test(std::path::Path::new("clip"), mime);
        m.kind = Kind::Video;
        m.video_codec = v.map(Into::into);
        m.audio_codec = a.map(Into::into);
        m
    }

    #[test]
    fn which_originals_tvs_play() {
        assert!(tv_plays_original(&video("video/mp4", Some("hevc"), Some("aac"))));
        assert!(tv_plays_original(&video("video/x-matroska", Some("h264"), Some(""))));
        assert!(!tv_plays_original(&video("video/mp4", Some("hevc"), Some("opus"))));
        assert!(!tv_plays_original(&video("video/x-msvideo", Some("h264"), Some("mp3"))));
        assert!(!tv_plays_original(&video("video/webm", Some("vp9"), Some("opus"))));
        assert!(!tv_plays_original(&video("video/mp4", None, None)), "not probed yet");
    }

    #[test]
    fn converts_to_transport_stream_and_fragmented_mp4() {
        if !ffmpeg::available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.mkv");
        // HEVC in MKV: the usual case a TV or browser can't play.
        ffmpeg::make_test_video(&src, &["-c:v", "libx265", "-x265-params", "log-level=none"]);
        let mut m = video("video/x-matroska", Some("hevc"), Some(""));
        m.path = src;

        for (container, check) in [
            (Container::Ts, (|b: &[u8]| b[0] == 0x47 && b[188] == 0x47) as fn(&[u8]) -> bool),
            (Container::Mp4, |b: &[u8]| &b[4..8] == b"ftyp"),
        ] {
            // The CPU encoder always exists, so the test doesn't depend on the PC's GPU.
            let mut cmd = command_with(&m, container, 0.5, Encoder::Cpu);
            cmd.stdin(std::process::Stdio::null());
            let mut child = cmd.spawn().unwrap();
            let mut out = Vec::new();
            child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
            assert!(child.wait().unwrap().success());
            assert!(out.len() > 1000 && check(&out), "{container:?} output looks wrong");
        }
    }

    #[test]
    fn hdr_is_converted_to_sdr() {
        if !ffmpeg::available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hdr.mp4");
        ffmpeg::make_hdr_video(&src, ffmpeg::Hdr::Pq);
        let mut m = video("video/mp4", Some("hevc"), Some(""));
        m.path = src;
        let out = dir.path().join("out.ts");
        let mut cmd = command_with(&m, Container::Ts, 0.0, Encoder::Cpu);
        cmd.stdout(std::fs::File::create(&out).unwrap());
        assert!(cmd.status().unwrap().success());

        let mut probe = ffmpeg::command("ffprobe");
        probe.args(["-v", "error", "-show_entries", "stream=pix_fmt,color_transfer,color_primaries", "-of", "json"]).arg(&out);
        let info: serde_json::Value = serde_json::from_slice(&ffmpeg::run(probe).unwrap()).unwrap();
        let s = &info["streams"][0];
        assert_eq!((s["pix_fmt"].as_str(), s["color_transfer"].as_str()), (Some("yuv420p"), Some("bt709")), "{s}");
        let px = ffmpeg::decode_frame(&out, None, 64).unwrap().to_rgb8().get_pixel(10, 10).0;
        assert!(px[0] > 170 && px[1] < 90, "colours survive: {px:?}");
    }

    #[test]
    fn picks_a_working_encoder() {
        if !ffmpeg::available() {
            return;
        }
        assert!(works(encoder()));
    }
}
