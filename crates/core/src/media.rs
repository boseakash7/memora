use std::path::{Path, PathBuf};

use r2d2_sqlite::rusqlite::types::Value;
use r2d2_sqlite::rusqlite::{OptionalExtension, Row, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::metadata::{self, Details};
use crate::places::PlaceName;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Photo,
    Video,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Photo => "photo",
            Kind::Video => "video",
        }
    }

    fn parse(s: &str) -> Kind {
        if s == "video" { Kind::Video } else { Kind::Photo }
    }
}

/// How a file gets decoded for thumbnails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// JPEG: decoded at reduced scale for fast thumbnails.
    Jpeg,
    /// PNG, GIF, WebP, BMP, TIFF: decoded with the `image` crate.
    Raster,
    /// HEIC, HEIF, AVIF: decoded with FFmpeg.
    FfmpegStill,
    /// Camera RAW: the embedded JPEG preview is shown (see `raw`).
    Raw,
    Video,
}

/// File types the library picks up: (extension, mime, format).
const TYPES: &[(&str, &str, Format)] = &[
    ("jpg", "image/jpeg", Format::Jpeg),
    ("jpeg", "image/jpeg", Format::Jpeg),
    ("jfif", "image/jpeg", Format::Jpeg),
    ("png", "image/png", Format::Raster),
    ("gif", "image/gif", Format::Raster),
    ("webp", "image/webp", Format::Raster),
    ("bmp", "image/bmp", Format::Raster),
    ("tif", "image/tiff", Format::Raster),
    ("tiff", "image/tiff", Format::Raster),
    ("heic", "image/heic", Format::FfmpegStill),
    ("heif", "image/heif", Format::FfmpegStill),
    ("avif", "image/avif", Format::FfmpegStill),
    ("dng", "image/x-adobe-dng", Format::Raw),
    ("cr2", "image/x-canon-cr2", Format::Raw),
    ("cr3", "image/x-canon-cr3", Format::Raw),
    ("nef", "image/x-nikon-nef", Format::Raw),
    ("nrw", "image/x-nikon-nrw", Format::Raw),
    ("arw", "image/x-sony-arw", Format::Raw),
    ("srf", "image/x-sony-srf", Format::Raw),
    ("sr2", "image/x-sony-sr2", Format::Raw),
    ("raf", "image/x-fuji-raf", Format::Raw),
    ("orf", "image/x-olympus-orf", Format::Raw),
    ("rw2", "image/x-panasonic-rw2", Format::Raw),
    ("pef", "image/x-pentax-pef", Format::Raw),
    ("srw", "image/x-samsung-srw", Format::Raw),
    ("rwl", "image/x-leica-rwl", Format::Raw),
    ("3fr", "image/x-hasselblad-3fr", Format::Raw),
    ("iiq", "image/x-phaseone-iiq", Format::Raw),
    ("erf", "image/x-epson-erf", Format::Raw),
    ("mp4", "video/mp4", Format::Video),
    ("m4v", "video/mp4", Format::Video),
    ("mov", "video/quicktime", Format::Video),
    ("mkv", "video/x-matroska", Format::Video),
    ("webm", "video/webm", Format::Video),
    ("avi", "video/x-msvideo", Format::Video),
    ("wmv", "video/x-ms-wmv", Format::Video),
    ("3gp", "video/3gpp", Format::Video),
    ("mts", "video/mp2t", Format::Video),
    ("m2ts", "video/mp2t", Format::Video),
    ("mpg", "video/mpeg", Format::Video),
    ("mpeg", "video/mpeg", Format::Video),
];

/// The file's (mime, format) if it is a photo or video the library handles.
/// Dot-files are skipped: Android marks deleted items as `.trashed-<time>-<name>`,
/// and macOS leaves `._<name>` metadata files on shared drives.
pub fn file_type(path: &Path) -> Option<(&'static str, Format)> {
    if path.file_name()?.to_str()?.starts_with('.') {
        return None;
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    TYPES.iter().find(|(e, _, _)| *e == ext).map(|&(_, mime, format)| (mime, format))
}

pub fn format_of_mime(mime: &str) -> Format {
    TYPES.iter().find(|(_, m, _)| *m == mime).map(|t| t.2).unwrap_or(Format::Raster)
}

/// Photos the desktop webview cannot show directly, so they get a converted JPEG.
pub fn needs_display_copy(mime: &str) -> bool {
    matches!(mime, "image/tiff" | "image/heic" | "image/heif") || format_of_mime(mime) == Format::Raw
}

/// Processing progress of an item. Items appear in the timeline from `Metadata` on.
pub struct Stage;

impl Stage {
    pub const NEW: i64 = 0;
    pub const METADATA: i64 = 1;
    pub const THUMBNAIL: i64 = 2;
    /// File hash, perceptual hash and place name.
    pub const HASH: i64 = 3;
    /// Last stage of the indexing pipeline. Recognition runs after it, separately.
    pub const DONE: i64 = Stage::HASH;
    /// Faces and CLIP vector done (by the recognition worker, see `ai`).
    pub const AI: i64 = 4;
}

/// Full record for one photo or video.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Media {
    pub id: i64,
    pub folder_id: i64,
    pub path: PathBuf,
    pub name: String,
    pub size: i64,
    pub mtime: i64,
    pub kind: Kind,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<i64>,
    pub orientation: u8,
    pub taken_at: String,
    pub tz_offset: Option<i32>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub favorite: bool,
    pub missing: bool,
    pub stage: i64,
    pub error: Option<String>,
    /// Videos: FFmpeg codec names (`h264`, `hevc`...). None until probed; audio `""` = no sound.
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub trashed: bool,
    /// In the Archive: shown only on the Archive page (and the TV/DLNA Archive).
    pub archived: bool,
}

/// The condition for items that the everyday lists show: on disk, not in the trash and
/// not archived. Every query that lists items uses this (or [`VISIBLE_M`]), so an archived
/// item can't leak into one list by accident. Items also need `stage >= METADATA`.
pub(crate) const VISIBLE: &str = "missing = 0 AND trashed_at IS NULL AND archived_at IS NULL";
/// [`VISIBLE`] for a query where the media table is `m`.
pub(crate) const VISIBLE_M: &str = "m.missing = 0 AND m.trashed_at IS NULL AND m.archived_at IS NULL";

pub(crate) const MEDIA_COLUMNS: &str = "id, folder_id, path, name, size, mtime, kind, mime, width, height, \
     duration_ms, orientation, taken_at, tz_offset, camera_make, camera_model, lat, lon, favorite, \
     missing, stage, error, video_codec, audio_codec, trashed_at IS NOT NULL, archived_at IS NOT NULL";

impl Media {
    pub(crate) fn from_row(r: &Row) -> r2d2_sqlite::rusqlite::Result<Self> {
        Ok(Media {
            id: r.get(0)?,
            folder_id: r.get(1)?,
            path: PathBuf::from(r.get::<_, String>(2)?),
            name: r.get(3)?,
            size: r.get(4)?,
            mtime: r.get(5)?,
            kind: Kind::parse(&r.get::<_, String>(6)?),
            mime: r.get(7)?,
            width: r.get(8)?,
            height: r.get(9)?,
            duration_ms: r.get(10)?,
            orientation: r.get(11)?,
            taken_at: r.get(12)?,
            tz_offset: r.get(13)?,
            camera_make: r.get(14)?,
            camera_model: r.get(15)?,
            lat: r.get(16)?,
            lon: r.get(17)?,
            favorite: r.get(18)?,
            missing: r.get(19)?,
            stage: r.get(20)?,
            error: r.get(21)?,
            video_codec: r.get(22)?,
            audio_codec: r.get(23)?,
            trashed: r.get(24)?,
            archived: r.get(25)?,
        })
    }

    pub fn format(&self) -> Format {
        format_of_mime(&self.mime)
    }

    /// A record for a file that is not in a library, for tests.
    #[cfg(test)]
    pub(crate) fn for_test(path: &Path, mime: &str) -> Media {
        let kind = if format_of_mime(mime) == Format::Video { Kind::Video } else { Kind::Photo };
        Media {
            id: 1,
            folder_id: 1,
            path: path.to_path_buf(),
            name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            size: 0,
            mtime: 0,
            kind,
            mime: mime.into(),
            width: None,
            height: None,
            duration_ms: None,
            orientation: 1,
            taken_at: String::new(),
            tz_offset: None,
            camera_make: None,
            camera_model: None,
            lat: None,
            lon: None,
            favorite: false,
            missing: false,
            stage: Stage::DONE,
            error: None,
            video_codec: None,
            audio_codec: None,
            trashed: false,
            archived: false,
        }
    }
}

pub fn get(db: &Db, id: i64) -> Result<Media> {
    db.conn()?
        .query_row(&format!("SELECT {MEDIA_COLUMNS} FROM media WHERE id = ?1"), [id], Media::from_row)
        .optional()?
        .ok_or(Error::NotFound)
}

/// File paths of items, in the order asked; unknown ids are skipped.
pub fn paths(db: &Db, ids: &[i64]) -> Result<Vec<PathBuf>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare_cached("SELECT path FROM media WHERE id = ?1")?;
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        if let Some(path) = stmt.query_row([id], |r| r.get::<_, String>(0)).optional()? {
            out.push(PathBuf::from(path));
        }
    }
    Ok(out)
}

/// What the info panel shows beyond the stored record.
#[derive(Debug, Clone, Serialize)]
pub struct Info {
    #[serde(flatten)]
    pub details: Details,
    pub place: Option<PlaceName>,
}

pub fn info(db: &Db, id: i64) -> Result<Info> {
    let m = get(db, id)?;
    let place = db
        .conn()?
        .query_row(
            "SELECT p.country, p.admin1, p.city FROM media m JOIN places p ON p.id = m.place_id WHERE m.id = ?1",
            [id],
            |r| Ok(PlaceName { country: r.get(0)?, admin1: r.get(1)?, city: r.get(2)? }),
        )
        .optional()?;
    Ok(Info { details: metadata::details(&m.path, m.format()), place })
}

/// One grid cell. Short field names keep the timeline response small for big libraries.
#[derive(Debug, Clone, Serialize)]
pub struct TimelineItem {
    pub id: i64,
    /// Width and height as displayed; 0 when unknown.
    pub w: u32,
    pub h: u32,
    /// Taken at, local time `YYYY-MM-DDTHH:MM:SS`.
    pub t: String,
    /// True for videos.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub v: bool,
    /// Video duration in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub d: Option<i64>,
    /// File modification time in seconds; changes the thumbnail URL when the file changes.
    pub m: i64,
    /// True for favorites.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub f: bool,
}

/// Which items a list shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Everything that is not in the trash or the archive.
    #[default]
    All,
    Favorites,
    /// Needs `Query::album`.
    Album,
    /// Archived items (not those in the trash).
    Archive,
    /// Trashed items, archived or not.
    Trash,
}

/// Selects items for a grid. Everything is optional and combines with AND.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Query {
    #[serde(default)]
    pub scope: Scope,
    pub album: Option<i64>,
    /// Free text: every word must match the start of a word in the path, camera or album names.
    pub q: Option<String>,
    /// First and last day to include, `YYYY-MM-DD`.
    pub from: Option<String>,
    pub to: Option<String>,
    /// `photo` or `video`.
    pub kind: Option<String>,
    /// Camera as listed by [`cameras`].
    pub camera: Option<String>,
    /// Items taken at this place (see `places`).
    pub place: Option<i64>,
    /// Items with GPS inside this box: `west,south,east,north` in degrees.
    /// A box with west > east crosses the antimeridian.
    pub bbox: Option<String>,
    /// Items showing this person.
    pub person: Option<i64>,
    /// More items that match the free text `q` besides the file names etc.: people with that
    /// name and pictures that look like it. Filled in by the server.
    #[serde(skip)]
    pub also: Vec<i64>,
}

const CAMERA_EXPR: &str = "trim(coalesce(camera_make, '') || ' ' || coalesce(camera_model, ''))";

/// `"word"* "word"*` for FTS5, or None if the text has no searchable words.
/// Words are runs of letters and digits, so no input can break the FTS syntax.
fn fts_match(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

fn parse_bbox(s: &str) -> Result<[f64; 4]> {
    let bad = || Error::invalid(format!("not a bounding box (west,south,east,north): {s}"));
    let parts: Vec<f64> = s.split(',').map(|p| p.trim().parse::<f64>().map_err(|_| bad())).collect::<Result<_>>()?;
    let [west, south, east, north]: [f64; 4] = parts.try_into().map_err(|_| bad())?;
    if parts_invalid(&[west, south, east, north]) || south > north {
        return Err(bad());
    }
    Ok([west, south, east, north])
}

fn parts_invalid(v: &[f64; 4]) -> bool {
    v.iter().any(|x| !x.is_finite()) || !(-90.0..=90.0).contains(&v[1]) || !(-90.0..=90.0).contains(&v[3])
}

fn check_day(day: &str) -> Result<()> {
    chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .map(|_| ())
        .map_err(|_| Error::invalid(format!("not a date (use YYYY-MM-DD): {day}")))
}

/// SQL condition and arguments selecting the items a query asks for.
fn filter(db: &Db, q: &Query) -> Result<(String, Vec<Value>)> {
    let mut clauses = vec!["missing = 0".to_owned(), "stage >= ?".to_owned()];
    let mut args: Vec<Value> = vec![Stage::METADATA.into()];

    clauses.push(
        match q.scope {
            Scope::All => VISIBLE.to_owned(),
            Scope::Favorites => format!("{VISIBLE} AND favorite = 1"),
            Scope::Album => format!("{VISIBLE} AND id IN (SELECT media_id FROM album_items WHERE album_id = ?)"),
            Scope::Archive => "trashed_at IS NULL AND archived_at IS NOT NULL".to_owned(),
            Scope::Trash => "trashed_at IS NOT NULL".to_owned(),
        },
    );
    if q.scope == Scope::Album {
        let album = q.album.ok_or_else(|| Error::invalid("an album id is required"))?;
        crate::albums::get(db, album)?; // 404 for unknown albums
        args.push(album.into());
    }

    const FTS: &str = "id IN (SELECT rowid FROM media_fts WHERE media_fts MATCH ?)";
    const ALSO: &str = "id IN (SELECT value FROM json_each(?))";
    match (q.q.as_deref().and_then(fts_match), q.also.is_empty()) {
        (Some(m), true) => {
            clauses.push(FTS.into());
            args.push(m.into());
        }
        (Some(m), false) => {
            clauses.push(format!("({FTS} OR {ALSO})"));
            args.push(m.into());
            args.push(serde_json::to_string(&q.also)?.into());
        }
        (None, false) => {
            clauses.push(ALSO.into());
            args.push(serde_json::to_string(&q.also)?.into());
        }
        (None, true) => {}
    }
    if let Some(from) = q.from.as_deref().filter(|s| !s.is_empty()) {
        check_day(from)?;
        clauses.push("taken_at >= ?".into());
        args.push(from.to_owned().into());
    }
    if let Some(to) = q.to.as_deref().filter(|s| !s.is_empty()) {
        check_day(to)?;
        clauses.push("substr(taken_at, 1, 10) <= ?".into());
        args.push(to.to_owned().into());
    }
    match q.kind.as_deref().unwrap_or("") {
        "" => {}
        k @ ("photo" | "video") => {
            clauses.push("kind = ?".into());
            args.push(k.to_owned().into());
        }
        other => return Err(Error::invalid(format!("unknown kind: {other}"))),
    }
    if let Some(camera) = q.camera.as_deref().filter(|s| !s.is_empty()) {
        clauses.push(format!("{CAMERA_EXPR} = ?"));
        args.push(camera.to_owned().into());
    }

    if let Some(person) = q.person {
        clauses.push("id IN (SELECT media_id FROM faces WHERE person_id = ?)".into());
        args.push(person.into());
    }
    if let Some(place) = q.place {
        clauses.push("place_id = ?".into());
        args.push(place.into());
    }
    if let Some(bbox) = q.bbox.as_deref().filter(|s| !s.is_empty()) {
        let [west, south, east, north] = parse_bbox(bbox)?;
        clauses.push("lat BETWEEN ? AND ?".into());
        args.push(south.into());
        args.push(north.into());
        clauses.push(if west <= east { "lon BETWEEN ? AND ?" } else { "(lon >= ? OR lon <= ?)" }.into());
        args.push(west.into());
        args.push(east.into());
    }

    Ok((clauses.join(" AND "), args))
}

/// Items matching the query, newest first.
pub fn query(db: &Db, q: &Query) -> Result<Vec<TimelineItem>> {
    let (filter, args) = filter(db, q)?;
    let sql = format!(
        "SELECT id, width, height, taken_at, kind, duration_ms, mtime, favorite FROM media
         WHERE {filter} ORDER BY taken_at DESC, id DESC"
    );
    let conn = db.conn()?;
    let mut stmt = conn.prepare_cached(&sql)?;
    let items = stmt
        .query_map(params_from_iter(args), |r| {
            Ok(TimelineItem {
                id: r.get(0)?,
                w: r.get::<_, Option<u32>>(1)?.unwrap_or(0),
                h: r.get::<_, Option<u32>>(2)?.unwrap_or(0),
                t: r.get(3)?,
                v: r.get::<_, String>(4)? == "video",
                d: r.get(5)?,
                m: r.get::<_, i64>(6)? / 1000,
                f: r.get(7)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(items)
}

/// Full records for one page of a query (newest first), and how many items match in all.
/// `limit` 0 means no limit.
pub fn page(db: &Db, q: &Query, offset: u64, limit: u64) -> Result<(Vec<Media>, u64)> {
    let (filter, args) = filter(db, q)?;
    let conn = db.conn()?;
    let total: i64 = conn.query_row(
        &format!("SELECT count(*) FROM media WHERE {filter}"),
        params_from_iter(args.iter()),
        |r| r.get(0),
    )?;
    let total = total as u64;
    let limit = if limit == 0 { -1 } else { limit as i64 };
    let sql = format!(
        "SELECT {MEDIA_COLUMNS} FROM media WHERE {filter}
         ORDER BY taken_at DESC, id DESC LIMIT {limit} OFFSET {offset}"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let items = stmt.query_map(params_from_iter(args.iter()), Media::from_row)?.collect::<std::result::Result<_, _>>()?;
    Ok((items, total))
}

/// An item that may be shown to others (TV, network): indexed, not missing and not in the
/// trash. Archived items only when `archive` is shared too.
pub fn get_visible(db: &Db, id: i64, archive: bool) -> Result<Media> {
    let m = get(db, id)?;
    if m.trashed || m.missing || m.stage < Stage::METADATA || (m.archived && !archive) {
        return Err(Error::NotFound);
    }
    Ok(m)
}

/// Fill in a video's codecs if they were never probed (videos indexed before they were stored).
/// Best effort: a file ffprobe can't read keeps `None`.
pub fn ensure_codecs(db: &Db, m: &mut Media) -> Result<()> {
    if m.kind != Kind::Video || m.video_codec.is_some() {
        return Ok(());
    }
    let Ok(probe) = crate::ffmpeg::probe(&m.path) else { return Ok(()) };
    let (video, audio) = metadata::codecs(&probe);
    let audio = audio.unwrap_or_default();
    db.conn()?.execute(
        "UPDATE media SET video_codec = ?2, audio_codec = ?3 WHERE id = ?1",
        r2d2_sqlite::rusqlite::params![m.id, video, audio],
    )?;
    m.video_codec = video;
    m.audio_codec = Some(audio);
    Ok(())
}

/// A geotagged item for the map: id, latitude, longitude.
pub type MapPoint = (i64, f64, f64);

/// Every visible item that has GPS coordinates.
pub fn map_points(db: &Db) -> Result<Vec<MapPoint>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT id, lat, lon FROM media
         WHERE {VISIBLE} AND stage >= ?1 AND lat IS NOT NULL AND lon IS NOT NULL"
    ))?;
    let points = stmt
        .query_map([Stage::METADATA], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(points)
}

/// Every visible item, newest first.
pub fn timeline(db: &Db) -> Result<Vec<TimelineItem>> {
    query(db, &Query::default())
}

/// Cameras that took visible items, for the search filter.
pub fn cameras(db: &Db) -> Result<Vec<String>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT DISTINCT {CAMERA_EXPR} AS cam FROM media
         WHERE {VISIBLE} AND cam != '' ORDER BY cam COLLATE NOCASE"
    ))?;
    Ok(stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_types() {
        assert_eq!(file_type(Path::new("a/IMG_1.JPG")), Some(("image/jpeg", Format::Jpeg)));
        assert_eq!(file_type(Path::new("clip.MoV")).unwrap().1, Format::Video);
        assert_eq!(file_type(Path::new("x.heic")).unwrap().1, Format::FfmpegStill);
        assert_eq!(file_type(Path::new("notes.txt")), None);
        assert_eq!(file_type(Path::new("noext")), None);
        assert_eq!(file_type(Path::new(".trashed-1715478456-IMG1.heic")), None);
        assert!(needs_display_copy("image/heic"));
        assert!(!needs_display_copy("image/jpeg"));
        assert_eq!(file_type(Path::new("IMG_1.CR2")), Some(("image/x-canon-cr2", Format::Raw)));
        assert_eq!(file_type(Path::new("a.dng")).unwrap().1, Format::Raw);
        assert!(needs_display_copy("image/x-adobe-dng"));
    }
}
