//! Thumbnails for the grid and browser-friendly copies of HEIC/TIFF photos.
//!
//! Cache layout: `cache/<kind>/<id / 1000>/<id>.jpg`, where kind is `thumb` (grid), `display`
//! (HEIC/TIFF for the viewer), `tv` (photos for TVs) or `tn` (tiny DLNA thumbnails).
//! Files are keyed by media id, so renames and moves keep their thumbnails.

use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::metadata::Orientation;
use image::{DynamicImage, GrayImage, RgbImage};

use crate::AppPaths;
use crate::ffmpeg;
use crate::media::{Format, Media, needs_display_copy};
use crate::{Error, Result};

/// Grid thumbnails are this tall (enough for ~260px rows on a 150% display)...
pub const THUMB_HEIGHT: u32 = 400;
/// ...but no wider than this, for panoramas.
pub const THUMB_MAX_WIDTH: u32 = 1600;
/// Longest side of converted HEIC/TIFF photos shown in the viewer.
pub const DISPLAY_MAX: u32 = 2560;
/// Photos for TVs fit in 4K UHD. TVs refuse HEIC and very large JPEGs, and many ignore
/// EXIF rotation, so they get an upright JPEG of at most this size.
pub const TV_MAX_W: u32 = 3840;
pub const TV_MAX_H: u32 = 2160;
/// DLNA's `JPEG_TN` thumbnails must fit in 160x160.
pub const SMALL_MAX: u32 = 160;
const JPEG_QUALITY: u8 = 82;
const CACHE_KINDS: &[&str] = &["thumb", "display", "tv", "tn"];

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
            Format::FfmpegStill => {
                return prepare_ffmpeg_still(paths, media).map(|_| path);
            }
            _ => decode_raster(&media.path, media.orientation)?,
        };
        let (w, h) = fit_within(img.width(), img.height(), DISPLAY_MAX, DISPLAY_MAX);
        write_jpeg(&img.thumbnail_exact(w, h), &path)?;
    }
    Ok(path)
}

/// A photo as a TV wants it: an upright JPEG no bigger than 4K. Small, unrotated JPEGs are
/// served as they are; HEIC/AVIF use their display copy (already upright, at most 2560 px).
pub fn ensure_tv(paths: &AppPaths, media: &Media) -> Result<PathBuf> {
    let (w, h) = (media.width.unwrap_or(0), media.height.unwrap_or(0));
    match media.format() {
        Format::Video => return Err(Error::media("videos have no TV photo")),
        Format::FfmpegStill => return ensure_display(paths, media),
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
    let (mw, mh) = if media.format() == Format::FfmpegStill { (DISPLAY_MAX, DISPLAY_MAX) } else { (TV_MAX_W, TV_MAX_H) };
    Some(fit_within(w, h, mw, mh))
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

/// Decode a HEIC/AVIF once with FFmpeg (stitching tiled photos), rotate it, and keep the
/// result as the display copy. Thumbnails are made from that copy.
pub fn prepare_ffmpeg_still(paths: &AppPaths, media: &Media) -> Result<()> {
    let still = ffmpeg::probe_still(&media.path)?;
    let img = ffmpeg::decode_still(&media.path, &still, DISPLAY_MAX)?;
    write_jpeg(&orient(img, media.orientation), &display_path(paths, media.id))
}

/// Generate the grid thumbnail.
pub fn make_thumb(paths: &AppPaths, media: &Media) -> Result<()> {
    let img = match media.format() {
        Format::Jpeg => decode_jpeg_scaled(&media.path, media.orientation, thumb_size)?,
        Format::Raster => decode_raster(&media.path, media.orientation)?,
        Format::FfmpegStill => {
            let display = display_path(paths, media.id);
            if !is_fresh(&display, media) {
                prepare_ffmpeg_still(paths, media)?;
            }
            decode_jpeg_scaled(&display, 1, thumb_size)?
        }
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
    let mut decoder = jpeg_decoder::Decoder::new(BufReader::new(File::open(path)?));
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
        None => decode_raster(path, orientation),
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
        JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).encode_image(&rgb)?;
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
    fn flatten_puts_transparency_on_white() {
        let img = DynamicImage::from(image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0])));
        assert_eq!(flatten(&img).get_pixel(0, 0).0, [255, 255, 255]);
    }
}
