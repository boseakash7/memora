//! Thumbnails for the grid and browser-friendly copies of HEIC, RAW and TIFF photos.
//!
//! Cache layout: `cache/<kind>/<id / 1000>/<id>.jpg`, where kind is `thumb` (grid), `display`
//! (HEIC/TIFF for the viewer), `tv` (photos for TVs), `tn` (tiny DLNA thumbnails) or `story`
//! (video storyboards for the viewer's seek-bar preview).
//! Files are keyed by media id, so renames and moves keep their thumbnails.

use std::fs::File;
use std::io::{BufReader, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use image::metadata::Orientation;
use image::{DynamicImage, GrayImage, RgbImage};

use crate::AppPaths;
use crate::{ffmpeg, raw};
use crate::media::{Format, Media, needs_display_copy};
use crate::{Error, Result};

/// Grid thumbnails are this tall (enough for ~260px rows on a 150% display)...
pub const THUMB_HEIGHT: u32 = 400;
/// ...but no wider than this, for panoramas.
pub const THUMB_MAX_WIDTH: u32 = 1600;
/// Longest side of converted HEIC/RAW/TIFF photos shown in the viewer.
pub const DISPLAY_MAX: u32 = 2560;
/// Photos for TVs fit in 4K UHD. TVs refuse HEIC and very large JPEGs, and many ignore
/// EXIF rotation, so they get an upright JPEG of at most this size.
pub const TV_MAX_W: u32 = 3840;
pub const TV_MAX_H: u32 = 2160;
/// DLNA's `JPEG_TN` thumbnails must fit in 160x160.
pub const SMALL_MAX: u32 = 160;
const JPEG_QUALITY: u8 = 82;
/// Video storyboards: at most this many frames (one per second for shorter videos)...
pub const STORYBOARD_MAX_FRAMES: u32 = 100;
/// ...in rows of this many...
pub const STORYBOARD_COLUMNS: u32 = 10;
/// ...each fitting in this square (sharp on a 150% display at the preview's 176 px).
pub const STORYBOARD_TILE: u32 = 240;
const CACHE_KINDS: &[&str] = &["thumb", "display", "tv", "tn", "story"];

fn cache_file(paths: &AppPaths, kind: &str, id: i64) -> PathBuf {
    paths.cache_dir.join(kind).join((id / 1000).to_string()).join(format!("{id}.jpg"))
}

pub fn thumb_path(paths: &AppPaths, id: i64) -> PathBuf {
    cache_file(paths, "thumb", id)
}

pub fn display_path(paths: &AppPaths, id: i64) -> PathBuf {
    cache_file(paths, "display", id)
}

/// Delete cached files for items that left the library.
pub fn remove_cached(paths: &AppPaths, ids: &[i64]) {
    for &id in ids {
        for kind in CACHE_KINDS {
            let _ = std::fs::remove_file(cache_file(paths, kind, id));
        }
    }
}

/// A cached file is fresh if it was written after the source file last changed.
fn is_fresh(cached: &Path, media: &Media) -> bool {
    let Ok(modified) = cached.metadata().and_then(|m| m.modified()) else { return false };
    let written = modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    written >= media.mtime
}

/// Path to an up-to-date thumbnail, generating it if needed.
pub fn ensure_thumb(paths: &AppPaths, media: &Media) -> Result<PathBuf> {
    let path = thumb_path(paths, media.id);
    if !is_fresh(&path, media) {
        make_thumb(paths, media)?;
    }
    Ok(path)
}

/// The file the viewer should show: the original, or a converted JPEG for HEIC/TIFF.
pub fn ensure_display(paths: &AppPaths, media: &Media) -> Result<PathBuf> {
    if !needs_display_copy(&media.mime) {
        return Ok(media.path.clone());
    }
    let path = display_path(paths, media.id);
    if !is_fresh(&path, media) {
        let img = match media.format() {
            Format::FfmpegStill | Format::Raw => {
                return prepare_still(paths, media).map(|_| path);
            }
            _ => decode_raster(&media.path, media.orientation)?,
        };
        let (w, h) = fit_within(img.width(), img.height(), DISPLAY_MAX, DISPLAY_MAX);
        write_jpeg(&img.thumbnail_exact(w, h), &path)?;
    }
    Ok(path)
}

/// A photo as a TV wants it: an upright JPEG no bigger than 4K. Small, unrotated JPEGs are
/// served as they are; HEIC/AVIF/RAW use their display copy (already upright, at most 2560 px).
pub fn ensure_tv(paths: &AppPaths, media: &Media) -> Result<PathBuf> {
    let (w, h) = (media.width.unwrap_or(0), media.height.unwrap_or(0));
    match media.format() {
        Format::Video => return Err(Error::media("videos have no TV photo")),
        Format::FfmpegStill | Format::Raw => return ensure_display(paths, media),
        Format::Jpeg if media.orientation <= 1 && w > 0 && w <= TV_MAX_W && h > 0 && h <= TV_MAX_H => {
            return Ok(media.path.clone());
        }
        _ => {}
    }
    let path = cache_file(paths, "tv", media.id);
    if !is_fresh(&path, media) {
        let fit = |w, h| fit_within(w, h, TV_MAX_W, TV_MAX_H);
        let img = match media.format() {
            Format::Jpeg => decode_jpeg_scaled(&media.path, media.orientation, fit)?,
            _ => decode_raster(&media.path, media.orientation)?,
        };
        let (w, h) = fit(img.width(), img.height());
        write_jpeg(&img.thumbnail_exact(w, h), &path)?;
    }
    Ok(path)
}

/// Pixel size of [`ensure_tv`]'s picture, from the stored (displayed) size.
pub fn tv_size(media: &Media) -> Option<(u32, u32)> {
    let (w, h) = (media.width?, media.height?);
    let (mw, mh) = match media.format() {
        Format::FfmpegStill | Format::Raw => (DISPLAY_MAX, DISPLAY_MAX),
        _ => (TV_MAX_W, TV_MAX_H),
    };
    Some(fit_within(w, h, mw, mh))
}

/// Pixel size of [`ensure_small`]'s picture, from the stored (displayed) size.
pub fn small_size(media: &Media) -> Option<(u32, u32)> {
    Some(fit_within(media.width?, media.height?, SMALL_MAX, SMALL_MAX))
}

/// A thumbnail that fits in 160x160 (DLNA `JPEG_TN`), made from the grid thumbnail.
pub fn ensure_small(paths: &AppPaths, media: &Media) -> Result<PathBuf> {
    let path = cache_file(paths, "tn", media.id);
    if !is_fresh(&path, media) {
        let thumb = ensure_thumb(paths, media)?;
        let img = decode_jpeg_scaled(&thumb, 1, |w, h| fit_within(w, h, SMALL_MAX, SMALL_MAX))?;
        let (w, h) = fit_within(img.width(), img.height(), SMALL_MAX, SMALL_MAX);
        write_jpeg(&img.thumbnail_exact(w, h), &path)?;
    }
    Ok(path)
}

/// Longest side of the picture face and scene recognition look at.
pub const ANALYSIS_MAX: u32 = 1600;

/// The item as shown (upright), at most [`ANALYSIS_MAX`] on its longest side, for recognition.
/// Videos use a frame a little way in, like their thumbnail.
pub fn analysis_image(paths: &AppPaths, media: &Media) -> Result<RgbImage> {
    let fit = |w, h| fit_within(w, h, ANALYSIS_MAX, ANALYSIS_MAX);
    let img = match media.format() {
        Format::Jpeg => decode_jpeg_scaled(&media.path, media.orientation, fit)?,
        Format::Raster => decode_raster(&media.path, media.orientation)?,
        Format::FfmpegStill | Format::Raw => decode_jpeg_scaled(&still_copy(paths, media)?, 1, fit)?,
        Format::Video => {
            let seek = media.duration_ms.filter(|d| *d > 3000).map(|_| 1.0);
            ffmpeg::decode_frame(&media.path, seek, ANALYSIS_MAX)?
        }
    };
    let (w, h) = fit(img.width(), img.height());
    let img = if (w, h) == (img.width(), img.height()) { img } else { img.thumbnail_exact(w, h) };
    Ok(flatten(&img))
}

/// Side of the square face pictures on the People pages.
pub const FACE_SIZE: u32 = 256;

pub fn face_path(paths: &AppPaths, face_id: i64) -> PathBuf {
    cache_file(paths, "face", face_id)
}

/// Save the square picture of a face: the face box (fractions of `img`) with some room around it.
pub fn write_face(paths: &AppPaths, face_id: i64, img: &RgbImage, rect: [f32; 4]) -> Result<()> {
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let [x, y, w, h] = [rect[0] * iw, rect[1] * ih, rect[2] * iw, rect[3] * ih];
    // A square around the face centre, 1.6x the face, kept inside the picture where possible.
    let side = (w.max(h) * 1.6).min(iw.min(ih)).max(1.0);
    let cx = (x + w / 2.0 - side / 2.0).clamp(0.0, (iw - side).max(0.0));
    let cy = (y + h / 2.0 - side / 2.0).clamp(0.0, (ih - side).max(0.0));
    let crop = image::imageops::crop_imm(img, cx as u32, cy as u32, side as u32, side as u32).to_image();
    let out = DynamicImage::from(crop).resize_exact(FACE_SIZE, FACE_SIZE, image::imageops::FilterType::CatmullRom);
    write_jpeg(&out, &face_path(paths, face_id))
}

/// Decode a HEIC/AVIF (with FFmpeg, stitching tiled photos) or a RAW file once, rotate it,
/// and keep the result as the display copy. Thumbnails and recognition use that copy.
pub fn prepare_still(paths: &AppPaths, media: &Media) -> Result<()> {
    let fit = |w, h| fit_within(w, h, DISPLAY_MAX, DISPLAY_MAX);
    let img = match media.format() {
        Format::Raw => {
            let raw = raw::probe(&media.path)?;
            match &raw.source {
                raw::Source::Jpeg(offset) => decode_jpeg_scaled_at(&media.path, *offset, media.orientation, fit)?,
                raw::Source::Cfa(cfa) => orient(raw::develop(&media.path, cfa)?.into(), media.orientation),
            }
        }
        _ => {
            // The layout from the file's boxes when it is certain: one FFmpeg run instead of two.
            let from_boxes = crate::heif::read(&media.path).and_then(|i| ffmpeg::Still::from_heif(&i));
            let still = match from_boxes {
                Some(still) => still,
                None => ffmpeg::probe_still(&media.path)?,
            };
            orient(ffmpeg::decode_still(&media.path, &still, DISPLAY_MAX)?, media.orientation)
        }
    };
    let (w, h) = fit(img.width(), img.height());
    let img = if (w, h) == (img.width(), img.height()) { img } else { img.thumbnail_exact(w, h) };
    write_jpeg(&img, &display_path(paths, media.id))
}

/// The up-to-date display copy of a HEIC/AVIF/RAW photo, made if needed.
fn still_copy(paths: &AppPaths, media: &Media) -> Result<PathBuf> {
    let display = display_path(paths, media.id);
    if !is_fresh(&display, media) {
        prepare_still(paths, media)?;
    }
    Ok(display)
}

/// How a video's storyboard is laid out. Tile `i` shows the video at `i * interval` seconds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Storyboard {
    pub frames: u32,
    pub interval: f64,
    pub cols: u32,
    pub rows: u32,
    pub tile_w: u32,
    pub tile_h: u32,
}

impl Storyboard {
    /// None for items that aren't videos or whose length or size is unknown.
    pub fn of(media: &Media) -> Option<Storyboard> {
        if media.format() != Format::Video {
            return None;
        }
        let secs = media.duration_ms.filter(|d| *d > 0)? as f64 / 1000.0;
        let (w, h) = (media.width.filter(|w| *w > 0)?, media.height.filter(|h| *h > 0)?);
        let frames = (secs.ceil() as u32).clamp(1, STORYBOARD_MAX_FRAMES);
        let cols = frames.min(STORYBOARD_COLUMNS);
        let (tw, th) = fit_within(w, h, STORYBOARD_TILE, STORYBOARD_TILE);
        Some(Storyboard {
            frames,
            interval: secs / frames as f64,
            cols,
            rows: frames.div_ceil(cols),
            // Even sizes keep FFmpeg's 4:2:0 scaling exact.
            tile_w: (tw & !1).max(2),
            tile_h: (th & !1).max(2),
        })
    }
}

/// Path to an up-to-date storyboard of a video, made if needed, with its layout.
pub fn ensure_storyboard(paths: &AppPaths, media: &Media) -> Result<(PathBuf, Storyboard)> {
    let layout = Storyboard::of(media).ok_or_else(|| Error::media("no storyboard for this item"))?;
    let path = cache_file(paths, "story", media.id);
    if !is_fresh(&path, media) {
        let tile = (layout.tile_w, layout.tile_h);
        let img = ffmpeg::storyboard(&media.path, layout.interval, tile, layout.cols, layout.rows)?;
        write_jpeg(&img, &path)?;
    }
    Ok((path, layout))
}

/// A video frame at `secs` at full size (rotation applied, HDR tone mapped), as PNG,
/// for "Copy this frame".
pub fn video_frame_png(media: &Media, secs: f64) -> Result<Vec<u8>> {
    if media.format() != Format::Video {
        return Err(Error::media("not a video"));
    }
    let img = ffmpeg::decode_frame(&media.path, Some(secs.max(0.0)), 8192)?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(flatten(&img)).write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

/// Generate the grid thumbnail.
pub fn make_thumb(paths: &AppPaths, media: &Media) -> Result<()> {
    let img = match media.format() {
        Format::Jpeg => decode_jpeg_scaled(&media.path, media.orientation, thumb_size)?,
        Format::Raster => decode_raster(&media.path, media.orientation)?,
        Format::FfmpegStill | Format::Raw => decode_jpeg_scaled(&still_copy(paths, media)?, 1, thumb_size)?,
        Format::Video => {
            // A frame a little way in avoids black fade-ins.
            let seek = media.duration_ms.filter(|d| *d > 3000).map(|_| 1.0);
            ffmpeg::decode_frame(&media.path, seek, THUMB_MAX_WIDTH)?
        }
    };
    let (w, h) = thumb_size(img.width(), img.height());
    write_jpeg(&img.thumbnail_exact(w, h), &thumb_path(paths, media.id))
}

/// Thumbnail size: `THUMB_HEIGHT` tall (never upscaled), capped at `THUMB_MAX_WIDTH` wide.
pub fn thumb_size(w: u32, h: u32) -> (u32, u32) {
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let mut th = h.min(THUMB_HEIGHT as f64);
    let mut tw = w * th / h;
    if tw > THUMB_MAX_WIDTH as f64 {
        tw = THUMB_MAX_WIDTH as f64;
        th = h * tw / w;
    }
    (tw.round().max(1.0) as u32, th.round().max(1.0) as u32)
}

fn fit_within(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let scale = (max_w as f64 / w.max(1) as f64).min(max_h as f64 / h.max(1) as f64).min(1.0);
    (((w as f64 * scale).round() as u32).max(1), ((h as f64 * scale).round() as u32).max(1))
}

fn orient(mut img: DynamicImage, orientation: u8) -> DynamicImage {
    if let Some(o) = Orientation::from_exif(orientation) {
        img.apply_orientation(o);
    }
    img
}

fn decode_raster(path: &Path, orientation: u8) -> Result<DynamicImage> {
    let img = image::ImageReader::open(path)?.with_guessed_format()?.decode()?;
    Ok(orient(img, orientation))
}

/// JPEGs can be decoded at 1/2, 1/4 or 1/8 scale, which makes thumbnails of
/// large photos several times faster than a full decode. `size` maps the displayed size to
/// the wanted size; the result is at least that big (callers resize the rest of the way).
fn decode_jpeg_scaled(path: &Path, orientation: u8, size: impl Fn(u32, u32) -> (u32, u32)) -> Result<DynamicImage> {
    decode_jpeg_scaled_at(path, 0, orientation, size)
}

/// [`decode_jpeg_scaled`] for a JPEG that starts `offset` bytes into the file (RAW previews).
fn decode_jpeg_scaled_at(
    path: &Path,
    offset: u64,
    orientation: u8,
    size: impl Fn(u32, u32) -> (u32, u32),
) -> Result<DynamicImage> {
    let open = || -> Result<BufReader<File>> {
        let mut r = BufReader::new(File::open(path)?);
        r.seek(SeekFrom::Start(offset))?;
        Ok(r)
    };
    let mut decoder = jpeg_decoder::Decoder::new(open()?);
    let scaled = (|| {
        decoder.read_info().ok()?;
        let info = decoder.info()?;
        let (raw_w, raw_h) = (u32::from(info.width), u32::from(info.height));
        // Target size in the file's own (unrotated) orientation.
        let swap = (5..=8).contains(&orientation);
        let (dw, dh) = if swap { (raw_h, raw_w) } else { (raw_w, raw_h) };
        let (tw, th) = size(dw, dh);
        let (tw, th) = if swap { (th, tw) } else { (tw, th) };
        let (w, h) = decoder.scale(tw as u16, th as u16).ok()?;
        let pixels = decoder.decode().ok()?;
        let (w, h) = (u32::from(w), u32::from(h));
        match decoder.info()?.pixel_format {
            jpeg_decoder::PixelFormat::RGB24 => RgbImage::from_raw(w, h, pixels).map(DynamicImage::from),
            jpeg_decoder::PixelFormat::L8 => GrayImage::from_raw(w, h, pixels).map(DynamicImage::from),
            _ => None, // CMYK and 16-bit: let the general decoder handle them
        }
    })();
    match scaled {
        Some(img) => Ok(orient(img, orientation)),
        None if offset == 0 => decode_raster(path, orientation),
        None => Ok(orient(image::ImageReader::with_format(open()?, image::ImageFormat::Jpeg).decode()?, orientation)),
    }
}

/// Write a JPEG atomically (temp file + rename) so readers never see half a file.
fn write_jpeg(img: &DynamicImage, dest: &Path) -> Result<()> {
    let rgb = flatten(img);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dest.with_extension(format!("{}-{n}.tmp", std::process::id()));
    let result = (|| {
        let mut out = BufWriter::new(File::create(&tmp)?);
        // `jpeg-encoder` uses SIMD and is several times faster than the `image` crate's
        // encoder (~20 ms instead of ~110 ms for a 2560 px display copy).
        let (w, h) = (u16::try_from(rgb.width()), u16::try_from(rgb.height()));
        let (Ok(w), Ok(h)) = (w, h) else { return Err(Error::media("picture too large for JPEG")) };
        jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY)
            .encode(rgb.as_raw(), w, h, jpeg_encoder::ColorType::Rgb)
            .map_err(|e| Error::media(format!("cannot write JPEG: {e}")))?;
        out.flush()?;
        drop(out);
        std::fs::rename(&tmp, dest)?;
        Ok::<_, Error>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// JPEG has no transparency: put transparent images on white.
fn flatten(img: &DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    let rgba = img.to_rgba8();
    RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
        let p = rgba.get_pixel(x, y).0;
        let a = p[3] as u32;
        let blend = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        image::Rgb([blend(p[0]), blend(p[1]), blend(p[2])])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_sizes() {
        assert_eq!(thumb_size(4000, 3000), (533, 400));
        assert_eq!(thumb_size(3000, 4000), (300, 400));
        assert_eq!(thumb_size(200, 100), (200, 100)); // never upscaled
        assert_eq!(thumb_size(10000, 1000), (1600, 160)); // panorama
        assert_eq!(fit_within(5000, 2500, 2560, 2560), (2560, 1280));
        assert_eq!(fit_within(100, 50, 2560, 2560), (100, 50));
    }

    #[test]
    fn jpeg_thumbnail_is_scaled_and_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("big.jpg");
        crate::metadata::test_jpeg_with_exif(&src, 3200, 1600); // landscape pixels, rotated 90°
        let img = decode_jpeg_scaled(&src, 6, thumb_size).unwrap();
        // Shown as portrait 1600x3200; the 200x400 thumbnail only needs a 1/8-scale decode.
        assert_eq!((img.width(), img.height()), (200, 400));
    }

    #[test]
    fn storyboard_layout() {
        let mut m = Media::for_test(Path::new("v.mp4"), "video/mp4");
        assert_eq!(Storyboard::of(&m), None); // length unknown
        (m.width, m.height, m.duration_ms) = (Some(1080), Some(1920), Some(81_493));
        let s = Storyboard::of(&m).unwrap();
        assert_eq!((s.frames, s.cols, s.rows, s.tile_w, s.tile_h), (82, 10, 9, 134, 240));
        assert!((s.interval - 81.493 / 82.0).abs() < 1e-9);
        m.duration_ms = Some(3_600_000); // an hour: capped, one frame every 36 s
        let s = Storyboard::of(&m).unwrap();
        assert_eq!((s.frames, s.rows, s.interval), (100, 10, 36.0));
        (m.width, m.height, m.duration_ms) = (Some(1920), Some(1080), Some(400));
        let s = Storyboard::of(&m).unwrap();
        assert_eq!((s.frames, s.cols, s.rows, s.tile_w, s.tile_h), (1, 1, 1, 240, 134));
        assert_eq!(Storyboard::of(&Media::for_test(Path::new("p.jpg"), "image/jpeg")), None);
    }

    #[test]
    fn storyboard_and_frame_of_a_video() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("clip.mp4");
        ffmpeg::make_test_video(&src, &["-c:v", "libx264", "-g", "5"]);
        let mut m = Media::for_test(&src, "video/mp4");
        (m.width, m.height, m.duration_ms) = (Some(320), Some(240), Some(2000));
        let paths = AppPaths::in_dir(&dir.path().join("data")).unwrap();

        let (path, layout) = ensure_storyboard(&paths, &m).unwrap();
        assert_eq!((layout.frames, layout.cols, layout.rows), (2, 2, 1));
        let img = image::open(&path).unwrap();
        assert_eq!((img.width(), img.height()), (480, 180));
        // testsrc's picture moves, so the two tiles differ.
        assert_ne!(img.crop_imm(0, 0, 240, 180).to_rgb8().into_raw(), img.crop_imm(240, 0, 240, 180).to_rgb8().into_raw());

        let png = video_frame_png(&m, 1.2).unwrap();
        let frame = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
        assert_eq!((frame.width(), frame.height()), (320, 240));
    }

    #[test]
    fn flatten_puts_transparency_on_white() {
        let img = DynamicImage::from(image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0])));
        assert_eq!(flatten(&img).get_pixel(0, 0).0, [255, 255, 255]);
    }

    /// Thumbnails of real files for checking by eye (rotation, colours), with timings:
    /// `PHOTOS_TEST_DIR=in PHOTOS_TEST_OUT=out cargo test -p photos-core --release -- --ignored --nocapture real_files`
    #[test]
    #[ignore]
    fn real_files() {
        let dir = std::env::var("PHOTOS_TEST_DIR").expect("set PHOTOS_TEST_DIR");
        let out = PathBuf::from(std::env::var("PHOTOS_TEST_OUT").expect("set PHOTOS_TEST_OUT"));
        std::fs::create_dir_all(&out).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::in_dir(tmp.path()).unwrap();
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        for (i, path) in entries.into_iter().enumerate() {
            let Some((mime, format)) = crate::media::file_type(&path) else { continue };
            let started = std::time::Instant::now();
            let meta = match crate::metadata::read(&path, format) {
                Ok(m) => m,
                Err(e) => {
                    println!("{}: metadata failed: {e}", path.display());
                    continue;
                }
            };
            let t_meta = started.elapsed();
            let mut m = Media::for_test(&path, mime);
            m.id = i as i64 + 1;
            m.orientation = meta.orientation;
            (m.width, m.height, m.duration_ms) = (meta.width, meta.height, meta.duration_ms);
            let result = make_thumb(&paths, &m);
            println!(
                "{}: {:?}x{:?} orientation {} taken {:?} {:?} camera {:?} | meta {:.0?} thumb {:.0?} {}",
                path.file_name().unwrap().to_string_lossy(),
                meta.width,
                meta.height,
                meta.orientation,
                meta.taken_at,
                meta.tz_offset,
                meta.camera_model,
                t_meta,
                started.elapsed() - t_meta,
                result.as_ref().err().map(|e| e.to_string()).unwrap_or_default(),
            );
            if result.is_ok() {
                let name = format!("{}.jpg", path.file_name().unwrap().to_string_lossy());
                std::fs::copy(thumb_path(&paths, m.id), out.join(name)).unwrap();
            }
        }
    }
}
