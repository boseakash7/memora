//! Reads capture date, size, camera and location from photos (EXIF) and videos (ffprobe).

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::OnceLock;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, Offset, TimeZone, Utc};
use exif::{In, Tag, Value};
use serde::Serialize;
use serde_json::Value as Json;

use crate::Result;
use crate::media::Format;
use crate::{ffmpeg, heif, raw};

pub const DATE_FORMAT: &str = "%Y-%m-%dT%H:%M:%S";

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Metadata {
    /// Size as displayed, i.e. after applying rotation.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: u8,
    /// Local wall-clock time the photo or video was taken.
    pub taken_at: Option<NaiveDateTime>,
    pub tz_offset: Option<i32>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub duration_ms: Option<i64>,
    /// Videos only: FFmpeg codec names, e.g. `hevc` and `aac`. Audio is `""` when there is no sound.
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
}

pub fn read(path: &Path, format: Format) -> Result<Metadata> {
    let mut meta = match format {
        Format::Video => {
            let mtime = path.metadata().and_then(|m| m.modified()).ok().map(DateTime::<Utc>::from);
            let probe = ffmpeg::probe(path)?;
            ffmpeg::remember_hdr(path, &probe);
            from_probe(&probe, mtime, &Local)
        }
        _ => read_photo(path, format)?,
    };
    // A photo's EXIF date is already local time; the GPS position can still tell its offset.
    if meta.tz_offset.is_none()
        && let (Some(taken), Some(lat), Some(lon)) = (meta.taken_at, meta.lat, meta.lon)
        && let Some(zone) = zone_at(lat, lon)
    {
        meta.tz_offset = zone.from_local_datetime(&taken).earliest().map(|t| offset_minutes(t.offset()));
    }
    Ok(meta)
}

/// The time zone at a place on Earth (offline lookup).
pub fn zone_at(lat: f64, lon: f64) -> Option<chrono_tz::Tz> {
    static FINDER: OnceLock<tzf_rs::DefaultFinder> = OnceLock::new();
    let name = FINDER.get_or_init(tzf_rs::DefaultFinder::new).get_tz_name(lon, lat);
    name.parse().ok()
}

fn offset_minutes(offset: &impl Offset) -> i32 {
    offset.fix().local_minus_utc() / 60
}

/// Extra facts for the info panel. They are read from the file when someone opens the
/// panel, not stored in the library, so older items need no re-indexing.
#[derive(Debug, Default, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Details {
    pub lens: Option<String>,
    pub iso: Option<u32>,
    pub f_number: Option<f64>,
    /// Shutter speed as shown on a camera: `1/250` or `2.5`, in seconds.
    pub exposure: Option<String>,
    /// Millimetres.
    pub focal_length: Option<f64>,
    pub flash: Option<bool>,
    pub software: Option<String>,
    /// Metres above sea level.
    pub altitude: Option<f64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub fps: Option<f64>,
    /// Bits per second, whole file.
    pub bitrate: Option<i64>,
}

/// Best effort: a file that cannot be read just has no extra details.
pub fn details(path: &Path, format: Format) -> Details {
    match format {
        Format::Video => ffmpeg::probe(path).map(|p| video_details(&p)).unwrap_or_default(),
        _ => read_tags(path, format).map(|t| photo_details(&t)).unwrap_or_default(),
    }
}

/// A photo's EXIF fields. Most files have one EXIF block; Canon CR3 has several.
struct Tags(Vec<exif::Exif>);

impl Tags {
    fn field(&self, tag: Tag) -> Option<&exif::Field> {
        if let [one] = self.0.as_slice() {
            return one.get_field(tag, In::PRIMARY);
        }
        // CR3 blocks are plain TIFFs, so their EXIF and GPS fields read as TIFF tags of the
        // same number. The numbers don't overlap between the blocks.
        self.0.iter().flat_map(|e| e.fields()).find(|f| f.ifd_num == In::PRIMARY && f.tag.number() == tag.number())
    }
}

/// EXIF of a file, if it has any (plenty of screenshots, PNGs and edited images don't).
fn read_tags(path: &Path, format: Format) -> Option<Tags> {
    if format == Format::Raw {
        return read_raw_tags(path);
    }
    let file = File::open(path).ok()?;
    exif::Reader::new().read_from_container(&mut BufReader::new(file)).ok().map(|e| Tags(vec![e]))
}

fn read_raw_tags(path: &Path) -> Option<Tags> {
    const HEAD: u64 = 4 << 20;
    let reader = exif::Reader::new();
    // TIFF-based RAW files (CR2, NEF, ARW, DNG...) are one big TIFF with the EXIF near the
    // start, so read only that much of a 25+ MB file, and all of it only if that fails.
    let mut head = Vec::new();
    File::open(path).ok()?.take(HEAD).read_to_end(&mut head).ok()?;
    let tiff = head.starts_with(&[b'I', b'I', 42, 0]) || head.starts_with(&[b'M', b'M', 0, 42]);
    let truncated = head.len() as u64 == HEAD;
    if let Ok(e) = reader.read_raw(head) {
        return Some(Tags(vec![e]));
    }
    if tiff && truncated && let Ok(e) = reader.read_raw(std::fs::read(path).ok()?) {
        return Some(Tags(vec![e]));
    }
    let blocks: Vec<_> = raw::cr3_tiff_blocks(path).into_iter().filter_map(|b| reader.read_raw(b).ok()).collect();
    if !blocks.is_empty() {
        return Some(Tags(blocks));
    }
    // Others (Fuji RAF, Olympus ORF...): the EXIF of the embedded preview, which describes
    // the same shot.
    let raw::Source::Jpeg(offset) = raw::probe(path).ok()?.source else { return None };
    let mut file = BufReader::new(File::open(path).ok()?);
    file.seek(SeekFrom::Start(offset)).ok()?;
    reader.read_from_container(&mut file).ok().map(|e| Tags(vec![e]))
}

fn photo_details(exif: &Tags) -> Details {
    let field = |tag| exif.field(tag);
    let ascii = |tag| field(tag).and_then(|f| ascii_value(&f.value));
    let rational = |tag| match &field(tag)?.value {
        Value::Rational(r) => r.first().filter(|x| x.denom != 0).copied(),
        _ => None,
    };
    let positive = |x: exif::Rational| Some(x.to_f64()).filter(|v| v.is_finite() && *v > 0.0);

    let exposure = rational(Tag::ExposureTime).and_then(|r| {
        let secs = positive(r)?;
        Some(if secs < 1.0 {
            format!("1/{}", (1.0 / secs).round() as u32)
        } else {
            format!("{}", (secs * 10.0).round() / 10.0)
        })
    });

    let altitude = rational(Tag::GPSAltitude).map(|r| r.to_f64()).map(|alt| {
        // Reference 1 means below sea level.
        if field(Tag::GPSAltitudeRef).and_then(|f| f.value.get_uint(0)) == Some(1) { -alt } else { alt }
    });

    Details {
        lens: ascii(Tag::LensModel),
        iso: field(Tag::PhotographicSensitivity).and_then(|f| f.value.get_uint(0)).filter(|iso| *iso > 0),
        f_number: rational(Tag::FNumber).and_then(positive),
        exposure,
        focal_length: rational(Tag::FocalLength).and_then(positive),
        flash: field(Tag::Flash).and_then(|f| f.value.get_uint(0)).map(|v| v & 1 == 1),
        software: ascii(Tag::Software),
        altitude: altitude.filter(|a| a.is_finite()),
        ..Default::default()
    }
}

/// First video and audio stream (cover pictures don't count) and their codec names.
fn main_stream<'a>(probe: &'a Json, kind: &str) -> Option<(Option<String>, &'a Json)> {
    probe["streams"]
        .as_array()?
        .iter()
        .find(|s| s["codec_type"] == kind && s["disposition"]["attached_pic"] != 1)
        .map(|s| (s["codec_name"].as_str().map(str::to_owned), s))
}

/// Video and audio codec names, e.g. (`hevc`, `aac`).
pub fn codecs(probe: &Json) -> (Option<String>, Option<String>) {
    (main_stream(probe, "video").and_then(|v| v.0), main_stream(probe, "audio").and_then(|a| a.0))
}

fn video_details(probe: &Json) -> Details {
    let codec = |kind: &str| main_stream(probe, kind);
    let video = codec("video");
    // "30000/1001"; 0/0 means unknown.
    let fps = video.as_ref().and_then(|(_, s)| {
        let rate = s["avg_frame_rate"].as_str().filter(|r| *r != "0/0").or(s["r_frame_rate"].as_str())?;
        let (n, d) = rate.split_once('/')?;
        let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
        (d > 0.0 && n > 0.0).then(|| (n / d * 100.0).round() / 100.0)
    });
    Details {
        video_codec: video.and_then(|(name, _)| name),
        audio_codec: codec("audio").and_then(|(name, _)| name),
        fps,
        bitrate: probe["format"]["bit_rate"].as_str().and_then(|b| b.parse().ok()).filter(|b| *b > 0),
        ..Default::default()
    }
}

/// Local time for a file's modification time; used when a file has no capture date.
pub fn local_time_from_millis(ms: i64) -> NaiveDateTime {
    DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .with_timezone(&Local)
        .naive_local()
}

// ---------- photos ----------

fn read_photo(path: &Path, format: Format) -> Result<Metadata> {
    let exif = read_tags(path, format);
    let mut meta = Metadata { orientation: 1, ..Default::default() };
    if let Some(exif) = &exif {
        apply_exif(&mut meta, exif);
    }

    let (w, h) = match format {
        Format::FfmpegStill => {
            // HEIC/AVIF: decoders show the pixels as stored, so we rotate ourselves. The
            // file's own `irot`/`imir` boxes win over EXIF (the HEIF rule); without them EXIF
            // applies, which is how older iPhone photos store it.
            if let Some(info) = heif::read(path) {
                if let Some(o) = info.orientation {
                    meta.orientation = o;
                }
                (info.width, info.height)
            } else {
                let still = ffmpeg::probe_still(path)?;
                if let Some(o) = still.orientation() {
                    meta.orientation = o;
                }
                (still.width, still.height)
            }
        }
        // The size of the preview we show (usually the full picture).
        Format::Raw => raw::probe(path).map(|r| (r.width, r.height))?,
        _ => image::ImageReader::open(path)?.with_guessed_format()?.into_dimensions()?,
    };
    let (w, h) = if (5..=8).contains(&meta.orientation) { (h, w) } else { (w, h) };
    meta.width = Some(w);
    meta.height = Some(h);
    Ok(meta)
}

fn apply_exif(meta: &mut Metadata, exif: &Tags) {
    let field = |tag| exif.field(tag);
    let ascii = |tag| field(tag).and_then(|f| ascii_value(&f.value));

    if let Some(o) = field(Tag::Orientation).and_then(|f| f.value.get_uint(0))
        && (1..=8).contains(&o)
    {
        meta.orientation = o as u8;
    }

    let dates = [
        (Tag::DateTimeOriginal, Tag::OffsetTimeOriginal),
        (Tag::DateTimeDigitized, Tag::OffsetTimeDigitized),
        (Tag::DateTime, Tag::OffsetTime),
    ];
    for (date_tag, offset_tag) in dates {
        let Some(raw) = field(date_tag).and_then(|f| ascii_bytes(&f.value)) else { continue };
        let Ok(mut dt) = exif::DateTime::from_ascii(raw) else { continue };
        if let Some(off) = field(offset_tag).and_then(|f| ascii_bytes(&f.value)) {
            let _ = dt.parse_offset(off);
        }
        if let Some(naive) = exif_naive(&dt) {
            meta.taken_at = Some(naive);
            meta.tz_offset = dt.offset.map(i32::from);
            break;
        }
    }

    meta.camera_make = ascii(Tag::Make);
    meta.camera_model = ascii(Tag::Model);

    let coord = |value_tag, ref_tag, negative: &str| {
        let v = dms(&field(value_tag)?.value)?;
        let r = ascii(ref_tag).unwrap_or_default();
        Some(if r.eq_ignore_ascii_case(negative) { -v } else { v })
    };
    if let (Some(lat), Some(lon)) =
        (coord(Tag::GPSLatitude, Tag::GPSLatitudeRef, "S"), coord(Tag::GPSLongitude, Tag::GPSLongitudeRef, "W"))
        && valid_coords(lat, lon)
    {
        meta.lat = Some(lat);
        meta.lon = Some(lon);
    }
}

fn ascii_bytes(v: &Value) -> Option<&[u8]> {
    match v {
        Value::Ascii(parts) => parts.iter().find(|p| !p.is_empty()).map(|p| p.as_slice()),
        _ => None,
    }
}

fn ascii_value(v: &Value) -> Option<String> {
    let s = String::from_utf8_lossy(ascii_bytes(v)?);
    let s = s.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    (!s.is_empty()).then(|| s.to_owned())
}

fn exif_naive(dt: &exif::DateTime) -> Option<NaiveDateTime> {
    if dt.year < 1900 {
        return None; // "0000:00:00 00:00:00" placeholders
    }
    NaiveDate::from_ymd_opt(dt.year.into(), dt.month.into(), dt.day.into())?.and_hms_opt(
        dt.hour.into(),
        dt.minute.into(),
        dt.second.into(),
    )
}

/// Degrees/minutes/seconds rationals to decimal degrees.
fn dms(v: &Value) -> Option<f64> {
    match v {
        Value::Rational(r) if !r.is_empty() => {
            let part = |i: usize| r.get(i).filter(|x| x.denom != 0).map(|x| x.to_f64()).unwrap_or(0.0);
            Some(part(0) + part(1) / 60.0 + part(2) / 3600.0)
        }
        _ => None,
    }
}

fn valid_coords(lat: f64, lon: f64) -> bool {
    lat.is_finite()
        && lon.is_finite()
        && lat.abs() <= 90.0
        && lon.abs() <= 180.0
        && !(lat == 0.0 && lon == 0.0)
}

// ---------- videos ----------

pub(crate) fn from_probe<Z: TimeZone>(probe: &Json, mtime: Option<DateTime<Utc>>, pc_zone: &Z) -> Metadata {
    let mut meta = Metadata { orientation: 1, ..Default::default() };
    let format = &probe["format"];
    let tags = &format["tags"];
    let tag = |key: &str| tags[key].as_str().map(str::trim).filter(|s| !s.is_empty());

    let video = probe["streams"].as_array().and_then(|streams| {
        streams.iter().find(|s| s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1)
    });
    if let Some(s) = video {
        let (w, h) = (s["width"].as_u64(), s["height"].as_u64());
        let rotation = s["side_data_list"]
            .as_array()
            .and_then(|list| list.iter().find_map(|d| d["rotation"].as_f64()))
            .or_else(|| s["tags"]["rotate"].as_str().and_then(|r| r.parse().ok()))
            .unwrap_or(0.0) as i64;
        if let (Some(w), Some(h)) = (w, h) {
            let (w, h) = if rotation.rem_euclid(180) == 90 { (h, w) } else { (w, h) };
            meta.width = Some(w as u32);
            meta.height = Some(h as u32);
        }
    }

    let (video_codec, audio_codec) = codecs(probe);
    meta.video_codec = video_codec;
    meta.audio_codec = Some(audio_codec.unwrap_or_default());

    let seconds = |v: &Json| v.as_str().and_then(|s| s.parse::<f64>().ok());
    meta.duration_ms = seconds(&format["duration"])
        .or_else(|| video.and_then(|s| seconds(&s["duration"])))
        .filter(|d| d.is_finite() && *d > 0.0)
        .map(|d| (d * 1000.0).round() as i64);

    meta.camera_make = tag("com.apple.quicktime.make").or(tag("com.android.manufacturer")).map(str::to_owned);
    meta.camera_model = tag("com.apple.quicktime.model").or(tag("com.android.model")).map(str::to_owned);

    if let Some((lat, lon)) = tag("com.apple.quicktime.location.ISO6709").or(tag("location")).and_then(iso6709)
        && valid_coords(lat, lon)
    {
        meta.lat = Some(lat);
        meta.lon = Some(lon);
    }

    // Apple's creationdate keeps the local time and offset.
    if let Some(dt) = tag("com.apple.quicktime.creationdate")
        .and_then(|s| DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z").ok())
        .filter(|dt| plausible(&dt.naive_local()))
    {
        meta.taken_at = Some(dt.naive_local());
        meta.tz_offset = Some(dt.offset().local_minus_utc() / 60);
    } else if let Some(written) = tag("creation_time")
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.naive_utc())
        .filter(plausible)
    {
        let place = meta.lat.zip(meta.lon);
        let (taken, offset) = video_time(written, place, mtime, meta.duration_ms, pc_zone);
        meta.taken_at = Some(taken);
        meta.tz_offset = offset;
    }
    meta
}

/// Local time and UTC offset (minutes) of a video from its `creation_time` as written.
///
/// That should be UTC, and is on phones, but many cameras (and some dashcams and action
/// cameras) write their clock's local time there. Those are recognised by the file's
/// modification time: a camera writes the file while recording, so it was last modified
/// within the video's length of the start time. If the start only lines up when read as
/// local time, it was written as local time.
///
/// The offset comes from the place the video was taken (GPS), else from this PC's time zone.
fn video_time<Z: TimeZone>(
    written: NaiveDateTime,
    place: Option<(f64, f64)>,
    mtime: Option<DateTime<Utc>>,
    duration_ms: Option<i64>,
    pc_zone: &Z,
) -> (NaiveDateTime, Option<i32>) {
    let zone = place.and_then(|(lat, lon)| zone_at(lat, lon));
    let fits_mtime = |start: DateTime<Utc>| {
        mtime.is_some_and(|m| {
            let secs = (m - start).num_seconds();
            (-120..=duration_ms.unwrap_or(0) / 1000 + 600).contains(&secs)
        })
    };
    let as_local_utc = |naive: &NaiveDateTime| match &zone {
        Some(z) => z.from_local_datetime(naive).earliest().map(|t| t.with_timezone(&Utc)),
        None => pc_zone.from_local_datetime(naive).earliest().map(|t| t.with_timezone(&Utc)),
    };
    let as_utc = written.and_utc();
    let written_as_local = !fits_mtime(as_utc) && as_local_utc(&written).is_some_and(fits_mtime);
    if written_as_local {
        let offset = match &zone {
            Some(z) => z.from_local_datetime(&written).earliest().map(|t| offset_minutes(t.offset())),
            None => pc_zone.from_local_datetime(&written).earliest().map(|t| offset_minutes(t.offset())),
        };
        return (written, offset);
    }
    match &zone {
        Some(z) => {
            let local = as_utc.with_timezone(z);
            (local.naive_local(), Some(offset_minutes(local.offset())))
        }
        // Shown in this PC's time zone; the offset there is a guess, so it is not stored.
        None => (as_utc.with_timezone(pc_zone).naive_local(), None),
    }
}

/// Cameras without a clock write 1904/1970 epoch dates; ignore those.
fn plausible(dt: &NaiveDateTime) -> bool {
    dt.date() > NaiveDate::from_ymd_opt(1971, 1, 1).unwrap()
}

/// Parse `+37.3349-122.0090+000.000/` (ISO 6709 decimal degrees).
fn iso6709(s: &str) -> Option<(f64, f64)> {
    let s = s.trim_end_matches('/');
    let starts: Vec<usize> = s.char_indices().filter(|(_, c)| *c == '+' || *c == '-').map(|(i, _)| i).collect();
    if starts.len() < 2 || starts[0] != 0 {
        return None;
    }
    let end = starts.get(2).copied().unwrap_or(s.len());
    let lat = s[starts[0]..starts[1]].parse().ok()?;
    let lon = s[starts[1]..end].parse().ok()?;
    Some((lat, lon))
}

/// Write a JPEG with an EXIF block: orientation 6 (rotated 90°), a capture date with
/// a +05:30 offset, a camera make and GPS for Bengaluru.
#[cfg(test)]
pub(crate) fn test_jpeg_with_exif(path: &Path, width: u32, height: u32) {
    use exif::{Field, Rational};
    let ascii = |tag, s: &str| Field { tag, ifd_num: In::PRIMARY, value: Value::Ascii(vec![s.as_bytes().to_vec()]) };
    let dms = |tag, d: u32, m: u32| Field {
        tag,
        ifd_num: In::PRIMARY,
        value: Value::Rational(vec![Rational::from((d, 1)), Rational::from((m, 1)), Rational::from((0, 1))]),
    };
    let rational = |tag, num: u32, denom: u32| Field {
        tag,
        ifd_num: In::PRIMARY,
        value: Value::Rational(vec![Rational::from((num, denom))]),
    };
    let fields = [
        Field { tag: Tag::Orientation, ifd_num: In::PRIMARY, value: Value::Short(vec![6]) },
        Field { tag: Tag::PhotographicSensitivity, ifd_num: In::PRIMARY, value: Value::Short(vec![400]) },
        Field { tag: Tag::Flash, ifd_num: In::PRIMARY, value: Value::Short(vec![1]) },
        rational(Tag::ExposureTime, 4, 1000),
        rational(Tag::FNumber, 18, 10),
        rational(Tag::FocalLength, 35, 1),
        ascii(Tag::LensModel, "EF35mm f/1.8"),
        ascii(Tag::DateTimeOriginal, "2021:06:15 09:30:00"),
        ascii(Tag::OffsetTimeOriginal, "+05:30"),
        ascii(Tag::Make, "Canon"),
        dms(Tag::GPSLatitude, 12, 58),
        ascii(Tag::GPSLatitudeRef, "N"),
        dms(Tag::GPSLongitude, 77, 35),
        ascii(Tag::GPSLongitudeRef, "E"),
    ];
    let mut writer = exif::experimental::Writer::new();
    for f in &fields {
        writer.push_field(f);
    }
    let mut tiff = std::io::Cursor::new(Vec::new());
    writer.write(&mut tiff, false).unwrap();
    let tiff = tiff.into_inner();

    let mut jpeg = Vec::new();
    image::RgbImage::from_pixel(width, height, image::Rgb([10, 120, 200]))
        .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .unwrap();
    // Insert an APP1 "Exif" segment right after the start-of-image marker.
    let len = (2 + 6 + tiff.len()) as u16;
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(b"Exif\0\0");
    out.extend_from_slice(&tiff);
    out.extend_from_slice(&jpeg[2..]);
    std::fs::write(path, out).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_exif_from_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cam.jpg");
        test_jpeg_with_exif(&path, 40, 20);
        let m = read(&path, Format::Jpeg).unwrap();
        assert_eq!(m.orientation, 6);
        assert_eq!((m.width, m.height), (Some(20), Some(40)), "size is swapped for rotated photos");
        assert_eq!(m.taken_at.unwrap().format(DATE_FORMAT).to_string(), "2021-06-15T09:30:00");
        assert_eq!(m.tz_offset, Some(330));
        assert_eq!(m.camera_make.as_deref(), Some("Canon"));
        assert!((m.lat.unwrap() - 12.9667).abs() < 1e-3 && (m.lon.unwrap() - 77.5833).abs() < 1e-3);
    }

    #[test]
    fn reads_exposure_details_from_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cam.jpg");
        test_jpeg_with_exif(&path, 40, 20);
        let d = details(&path, Format::Jpeg);
        assert_eq!(d.lens.as_deref(), Some("EF35mm f/1.8"));
        assert_eq!((d.iso, d.f_number, d.focal_length), (Some(400), Some(1.8), Some(35.0)));
        assert_eq!(d.exposure.as_deref(), Some("1/250"), "4/1000 s is shown as a fraction of a second");
        assert_eq!(d.flash, Some(true));
        assert_eq!(d.video_codec, None);
    }

    #[test]
    fn details_of_a_file_without_exif_are_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        image::RgbImage::new(30, 20).save(&path).unwrap();
        assert_eq!(details(&path, Format::Raster), Details::default());
        assert_eq!(details(&dir.path().join("missing.jpg"), Format::Jpeg), Details::default());
    }

    #[test]
    fn video_details_from_probe() {
        let probe = json!({
            "streams": [
                {"codec_type": "video", "codec_name": "cover", "disposition": {"attached_pic": 1}},
                {"codec_type": "video", "codec_name": "h264", "avg_frame_rate": "30000/1001"},
                {"codec_type": "audio", "codec_name": "aac"}
            ],
            "format": {"bit_rate": "8123456"}
        });
        let d = video_details(&probe);
        assert_eq!((d.video_codec.as_deref(), d.audio_codec.as_deref()), (Some("h264"), Some("aac")));
        assert_eq!((d.fps, d.bitrate), (Some(29.97), Some(8_123_456)));

        let unknown = video_details(&json!({"streams": [{"codec_type": "video", "avg_frame_rate": "0/0"}]}));
        assert_eq!((unknown.fps, unknown.audio_codec, unknown.bitrate), (None, None, None));
    }

    #[test]
    fn details_of_a_real_video() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.mp4");
        ffmpeg::make_test_video(&path, &["-f", "lavfi", "-i", "sine=duration=2", "-c:v", "libx264", "-c:a", "aac", "-shortest"]);
        let d = details(&path, Format::Video);
        assert_eq!((d.video_codec.as_deref(), d.audio_codec.as_deref()), (Some("h264"), Some("aac")));
        assert_eq!(d.fps, Some(10.0));
        assert!(d.bitrate.is_some_and(|b| b > 0), "{d:?}");
    }

    #[test]
    fn parses_iso6709() {
        assert_eq!(iso6709("+37.3349-122.0090+000.000/"), Some((37.3349, -122.009)));
        assert_eq!(iso6709("-33.8688+151.2093/"), Some((-33.8688, 151.2093)));
        assert_eq!(iso6709("garbage"), None);
    }

    #[test]
    fn probe_rotated_iphone_video() {
        let probe = json!({
            "streams": [
                {"codec_type": "audio"},
                {"codec_type": "video", "width": 1920, "height": 1080,
                 "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}],
                 "disposition": {"attached_pic": 0}}
            ],
            "format": {
                "duration": "12.345",
                "tags": {
                    "creation_time": "2023-07-04T12:34:56.000000Z",
                    "com.apple.quicktime.creationdate": "2023-07-04T14:34:56+0200",
                    "com.apple.quicktime.make": "Apple",
                    "com.apple.quicktime.model": "iPhone 14",
                    "com.apple.quicktime.location.ISO6709": "+48.8584+002.2945+035.000/"
                }
            }
        });
        let m = from_probe(&probe, None, &Utc);
        assert_eq!((m.width, m.height), (Some(1080), Some(1920)));
        assert_eq!(m.duration_ms, Some(12345));
        assert_eq!(m.taken_at.unwrap().format(DATE_FORMAT).to_string(), "2023-07-04T14:34:56");
        assert_eq!(m.tz_offset, Some(120));
        assert_eq!(m.camera_model.as_deref(), Some("iPhone 14"));
        assert_eq!((m.lat, m.lon), (Some(48.8584), Some(2.2945)));
    }

    #[test]
    fn probe_ignores_epoch_dates_and_cover_art() {
        let probe = json!({
            "streams": [
                {"codec_type": "video", "width": 600, "height": 600, "disposition": {"attached_pic": 1}},
                {"codec_type": "video", "width": 640, "height": 480, "tags": {"rotate": "180"}}
            ],
            "format": {"tags": {"creation_time": "1970-01-01T00:00:00.000000Z"}}
        });
        let m = from_probe(&probe, None, &Utc);
        assert_eq!((m.width, m.height), (Some(640), Some(480)));
        assert_eq!(m.taken_at, None);
        assert_eq!(m.duration_ms, None);
    }

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, DATE_FORMAT).unwrap()
    }

    #[test]
    fn video_dates_in_utc_are_shown_in_local_time() {
        let india = chrono::FixedOffset::east_opt(330 * 60).unwrap();
        let utc = at("2023-01-01T10:00:00");
        // A phone video: written as UTC, file saved when recording stopped 30 s later.
        let mtime = Some(utc.and_utc() + chrono::Duration::seconds(30));
        assert_eq!(video_time(utc, None, mtime, Some(30_000), &india), (at("2023-01-01T15:30:00"), None));
        // Copied to the PC much later: the modification time says nothing, UTC is assumed.
        let copied = Some(utc.and_utc() + chrono::Duration::days(200));
        assert_eq!(video_time(utc, None, copied, Some(30_000), &india).0, at("2023-01-01T15:30:00"));
    }

    #[test]
    fn video_dates_a_camera_wrote_in_local_time_are_recognised() {
        let india = chrono::FixedOffset::east_opt(330 * 60).unwrap();
        // The camera's clock said 15:30 (India) and it wrote that as if it were UTC.
        let written = at("2023-01-01T15:30:00");
        let mtime = Some(at("2023-01-01T10:01:00").and_utc());
        assert_eq!(video_time(written, None, mtime, Some(60_000), &india), (written, Some(330)));
    }

    #[test]
    fn video_dates_use_the_time_zone_where_they_were_taken() {
        let india = chrono::FixedOffset::east_opt(330 * 60).unwrap();
        // Filmed in Paris in summer (UTC+2), looked at on a PC in India.
        let paris = Some((48.8584, 2.2945));
        let utc = at("2023-07-04T12:34:56");
        assert_eq!(video_time(utc, paris, None, None, &india), (at("2023-07-04T14:34:56"), Some(120)));
        // A Paris camera that writes local time.
        let mtime = Some(at("2023-07-04T10:35:00").and_utc());
        assert_eq!(video_time(utc, paris, mtime, Some(10_000), &india), (utc, Some(120)));
    }

    #[test]
    fn photos_with_gps_get_their_offset() {
        assert_eq!(zone_at(40.7128, -74.0060), Some(chrono_tz::America::New_York));
        assert_eq!(zone_at(22.57, 88.36), Some(chrono_tz::Asia::Kolkata));
    }

    #[test]
    fn photo_without_exif_still_has_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        image::RgbImage::new(30, 20).save(&path).unwrap();
        let m = read(&path, Format::Raster).unwrap();
        assert_eq!((m.width, m.height, m.orientation), (Some(30), Some(20), 1));
        assert_eq!(m.taken_at, None);
    }
}
