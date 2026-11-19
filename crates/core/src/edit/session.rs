//! What the editor needs from the library side: quick previews (the decoded photo is kept
//! for the few photos being edited), Enhance values, subject masks, and exporting a copy.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use image::{GrayImage, RgbImage};

use super::filters::Scene;
use super::{Adjust, Edit, auto, render};
use crate::media::{Format, Media};
use crate::{AppPaths, Error, Result, thumbs};

/// Longest side of the photo the editor previews are made from.
pub const SOURCE_MAX: u32 = 2560;
/// Previews are at most this big.
pub const PREVIEW_MAX: u32 = 2560;
const KEEP: usize = 3;

/// Longest sides of the smaller copies: for filter tiles, and for quick previews while a
/// slider moves. Rendering from a copy near the wanted size skips shrinking a big one.
const SMALL_MAX: u32 = 720;
const MID_MAX: u32 = 1280;

/// A photo decoded for the editor, with smaller copies of it.
#[derive(Clone)]
pub struct Source {
    pub full: Arc<RgbImage>,
    pub mid: Arc<RgbImage>,
    pub small: Arc<RgbImage>,
}

fn shrunk(img: &RgbImage, long: u32) -> RgbImage {
    let k = (long as f32 / img.width().max(img.height()) as f32).min(1.0);
    let (w, h) = (((img.width() as f32 * k).round() as u32).max(1), ((img.height() as f32 * k).round() as u32).max(1));
    image::imageops::thumbnail(img, w, h)
}

impl Source {
    /// The smallest copy big enough for a render that fills `box_w` x `box_h`.
    pub fn for_box(&self, box_w: u32, box_h: u32) -> &RgbImage {
        let fits = |img: &RgbImage| (box_w as f32 / img.width() as f32).min(box_h as f32 / img.height() as f32) <= 1.0;
        [&self.small, &self.mid].into_iter().find(|i| fits(i)).map(|a| a.as_ref()).unwrap_or(&self.full)
    }
}

/// Decoded photos for the editor, newest first.
#[derive(Default)]
pub struct Sources {
    entries: Mutex<VecDeque<(i64, i64, Source)>>,
}

impl Sources {
    /// The photo as displayed (unedited), at most [`SOURCE_MAX`].
    pub fn get(&self, paths: &AppPaths, media: &Media) -> Result<Source> {
        if media.format() == Format::Video {
            return Err(Error::media("videos can't be edited"));
        }
        {
            let mut e = self.entries.lock().unwrap();
            if let Some(i) = e.iter().position(|(id, mtime, _)| *id == media.id && *mtime == media.mtime) {
                let hit = e.remove(i).expect("found");
                let img = hit.2.clone();
                e.push_front(hit);
                return Ok(img);
            }
        }
        let full = thumbs::editor_source(paths, media, SOURCE_MAX)?;
        let mid = shrunk(&full, MID_MAX);
        let small = Arc::new(shrunk(&mid, SMALL_MAX));
        let img = Source { full: Arc::new(full), mid: Arc::new(mid), small };
        let mut e = self.entries.lock().unwrap();
        e.retain(|(id, _, _)| *id != media.id);
        e.push_front((media.id, media.mtime, img.clone()));
        e.truncate(KEEP);
        Ok(img)
    }
}

/// A preview of `edit` as JPEG, fitting in `max` x `max`.
pub fn preview(paths: &AppPaths, sources: &Sources, media: &Media, edit: &Edit, max: u32) -> Result<Vec<u8>> {
    let source = sources.get(paths, media)?;
    let max = max.clamp(16, PREVIEW_MAX);
    // The size the source must have for this preview, as displayed (see `Edit::source_box`).
    let (ow, oh) = edit.output_size(source.full.width(), source.full.height());
    let k = (max as f32 / ow.max(oh) as f32).min(1.0);
    let (bw, bh) = edit.source_box((ow as f32 * k).ceil() as u32, (oh as f32 * k).ceil() as u32);
    let src = source.for_box(bw, bh);
    let mask = if edit.blur > 0.0 { thumbs::load_mask(paths, media) } else { None };
    let img = render::render(src, mask.as_ref(), edit, max, max);
    thumbs::encode_jpeg(&img, 88)
}

/// Enhance's slider values for the photo as `edit` frames it.
pub fn enhance(paths: &AppPaths, sources: &Sources, media: &Media, edit: &Edit, scene: Option<Scene>) -> Result<Adjust> {
    let src = sources.get(paths, media)?;
    Ok(auto::enhance(&src.small, edit, scene))
}

/// Make the subject mask for background blur if there is none yet. `find` runs the model
/// on the photo (as displayed, at most 1024 px).
pub fn ensure_mask(
    paths: &AppPaths,
    media: &Media,
    find: impl FnOnce(&RgbImage) -> std::result::Result<GrayImage, String>,
) -> Result<()> {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    let _one = ONE_AT_A_TIME.lock().unwrap();
    if thumbs::has_mask(paths, media) {
        return Ok(());
    }
    let src = thumbs::editor_source(paths, media, 1024)?;
    let mask = find(&src).map_err(Error::media)?;
    thumbs::write_mask(paths, media.id, &mask)
}

/// Save the edited photo as a new JPEG at `dest` (full size; HEIC and RAW at the size of
/// their converted copy). The original is never written. Returns the size.
pub fn export(paths: &AppPaths, media: &Media, edit: &Edit, dest: &Path, quality: u8) -> Result<(u32, u32)> {
    let same = |a: &Path, b: &Path| match (dunce::canonicalize(a), dunce::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if same(dest, &media.path) {
        return Err(Error::Invalid("choose another name: the original is never changed".into()));
    }
    let img = thumbs::render_full(paths, media, edit)?;
    let (w, h) = (u16::try_from(img.width()), u16::try_from(img.height()));
    let (Ok(w), Ok(h)) = (w, h) else { return Err(Error::media("picture too large for JPEG")) };
    let tmp = dest.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| {
        let file = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        let mut enc = jpeg_encoder::Encoder::new(file, quality.clamp(50, 100));
        let exif = exif_block(&media.taken_at, media.camera_make.as_deref(), media.camera_model.as_deref());
        enc.add_app_segment(1, exif).map_err(|e| Error::media(format!("cannot write JPEG: {e}")))?;
        enc.encode(img.as_raw(), w, h, jpeg_encoder::ColorType::Rgb)
            .map_err(|e| Error::media(format!("cannot write JPEG: {e}")))?;
        std::fs::rename(&tmp, dest)?;
        Ok::<_, Error>(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|_| (img.width(), img.height()))
}

/// A minimal EXIF block for an exported photo: upright, when it was taken, camera.
/// So the copy sorts next to the original when it lands in a watched folder.
pub(crate) fn exif_block(taken_at: &str, make: Option<&str>, model: Option<&str>) -> Vec<u8> {
    // "2024-05-01T10:20:30" -> "2024:05:01 10:20:30"
    let date = (taken_at.len() >= 19).then(|| {
        let d = &taken_at[..19];
        format!("{}:{}:{} {}", &d[0..4], &d[5..7], &d[8..10], &d[11..19])
    });
    // Values longer than 4 bytes live after their IFD, and TIFF wants them at even offsets:
    // the count says how long the text is, the padding is extra.
    let ascii = |s: &str| {
        let mut v: Vec<u8> = s.bytes().filter(|b| b.is_ascii() && *b >= 0x20).take(60).collect();
        v.push(0);
        v
    };
    let padded = |v: &[u8]| v.len() + v.len() % 2;
    // IFD0: Make, Model, Orientation, ExifIFD pointer; Exif IFD: DateTimeOriginal.
    let mut ifd0: Vec<(u16, u16, Vec<u8>)> = Vec::new(); // tag, type, value bytes
    if let Some(m) = make.filter(|s| !s.is_empty()) {
        ifd0.push((0x010f, 2, ascii(m)));
    }
    if let Some(m) = model.filter(|s| !s.is_empty()) {
        ifd0.push((0x0110, 2, ascii(m)));
    }
    ifd0.push((0x0112, 3, 1u16.to_le_bytes().to_vec()));
    let exif_ifd: Vec<(u16, u16, Vec<u8>)> = date.iter().map(|d| (0x9003, 2, ascii(d))).collect();
    if !exif_ifd.is_empty() {
        ifd0.push((0x8769, 4, vec![0; 4])); // filled in below
    }

    let ifd_len = |n: usize| 2 + n * 12 + 4;
    let extra_len = |entries: &[(u16, u16, Vec<u8>)]| entries.iter().map(|e| if e.2.len() > 4 { padded(&e.2) } else { 0 }).sum::<usize>();
    let ifd0_at = 8usize;
    let ifd0_data_at = ifd0_at + ifd_len(ifd0.len());
    let exif_at = ifd0_data_at + extra_len(&ifd0);
    let exif_data_at = exif_at + ifd_len(exif_ifd.len());
    if let Some(e) = ifd0.iter_mut().find(|e| e.0 == 0x8769) {
        e.2 = (exif_at as u32).to_le_bytes().to_vec();
    }

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"II*\0");
    tiff.extend_from_slice(&(ifd0_at as u32).to_le_bytes());
    let write_ifd = |tiff: &mut Vec<u8>, entries: &mut [(u16, u16, Vec<u8>)], data_at: usize| {
        entries.sort_by_key(|e| e.0);
        tiff.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        let mut data = Vec::new();
        for (tag, typ, value) in entries.iter() {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&typ.to_le_bytes());
            let count = match typ {
                3 => value.len() / 2, // SHORT
                4 => value.len() / 4, // LONG
                _ => value.len(),     // ASCII
            };
            tiff.extend_from_slice(&(count as u32).to_le_bytes());
            if value.len() > 4 {
                tiff.extend_from_slice(&((data_at + data.len()) as u32).to_le_bytes());
                data.extend_from_slice(value);
                if value.len() % 2 == 1 {
                    data.push(0);
                }
            } else {
                let mut v = value.clone();
                v.resize(4, 0);
                tiff.extend_from_slice(&v);
            }
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&data);
    };
    write_ifd(&mut tiff, &mut ifd0, ifd0_data_at);
    let mut exif_ifd = exif_ifd;
    if !exif_ifd.is_empty() {
        write_ifd(&mut tiff, &mut exif_ifd, exif_data_at);
    }
    let mut out = b"Exif\0\0".to_vec();
    out.extend_from_slice(&tiff);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_photos_keep_their_date_and_camera() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::in_dir(&dir.path().join("data")).unwrap();
        let src = dir.path().join("p.png");
        RgbImage::from_pixel(60, 40, image::Rgb([120, 130, 140])).save(&src).unwrap();
        let mut m = Media::for_test(&src, "image/png");
        (m.width, m.height) = (Some(60), Some(40));
        m.taken_at = "2023-07-14T18:05:09".into();
        m.camera_make = Some("Canon".into());
        m.camera_model = Some("EOS R6".into());
        let edit = Edit { rotate: 1, ..Default::default() };
        assert!(export(&paths, &m, &edit, &src, 90).is_err(), "never over the original");

        let dest = dir.path().join("p-edited.jpg");
        assert_eq!(export(&paths, &m, &edit, &dest, 90).unwrap(), (40, 60));
        let meta = crate::metadata::read(&dest, Format::Jpeg).unwrap();
        assert_eq!(meta.orientation, 1);
        assert_eq!(meta.camera_model.as_deref(), Some("EOS R6"));
        assert_eq!(meta.taken_at.map(|t| t.to_string()).as_deref(), Some("2023-07-14 18:05:09"));
        let img = image::open(&dest).unwrap();
        assert_eq!((img.width(), img.height()), (40, 60));
    }
}
