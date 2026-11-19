//! Memories: short stories made from the library on their own, like "3 years ago",
//! "Trip to Gangtok", "Asha over the years" and "Best of 2025".
//!
//! Built from what the index already holds (dates, places, GPS, named people, perceptual
//! hashes) in one pass over the visible items, so archived, trashed and locked-folder items
//! are never in one. Each memory has a stable key (`day:2023-11-18`, `trip:2023-10-02`,
//! `person:<uid>`, `year:2025`, `month:2026-10`), so "seen", "hidden" and photos the user
//! removed survive rebuilds.

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Utc};
use r2d2_sqlite::rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::db::Db;
use crate::hash::hamming;
use crate::media::{Stage, TimelineItem, VISIBLE, VISIBLE_M};
use crate::places::haversine_km;
use crate::{Error, Result};

/// What kind of story a memory tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The same days in an earlier year.
    Day,
    Trip,
    Person,
    /// Best of a year.
    Year,
    /// Highlights of a month.
    Month,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Day => "day",
            Kind::Trip => "trip",
            Kind::Person => "person",
            Kind::Year => "year",
            Kind::Month => "month",
        }
    }

    fn parse(s: &str) -> Kind {
        match s {
            "trip" => Kind::Trip,
            "person" => Kind::Person,
            "year" => Kind::Year,
            "month" => Kind::Month,
            _ => Kind::Day,
        }
    }
}

// Rules (see plan.md, "Planned: Memories").
const DAY_WINDOW: i64 = 3;
const DAY_MIN: usize = 3;
const DAY_MAX: usize = 20;
const TRIP_KM: f64 = 100.0;
const TRIP_MIN_ITEMS: usize = 8;
const TRIP_ONE_DAY_MIN: usize = 20;
const TRIP_MAX: usize = 40;
const PERSON_MIN: usize = 12;
const PERSON_YEAR_MIN: usize = 15;
const PERSON_MAX: usize = 30;
const YEAR_MIN: usize = 40;
const YEAR_MAX: usize = 50;
const MONTH_MIN: usize = 25;
const MONTH_MAX: usize = 40;
/// Months back that get "highlights".
const MONTHS_BACK: u32 = 3;
/// Near-duplicates: perceptual hashes this close, taken this close together.
const SIMILAR_BITS: u32 = 10;
const SIMILAR_SECS: i64 = 600;

const MONTH_NAMES: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// One visible item, as the rules see it.
#[derive(Debug, Clone)]
struct Cand {
    id: i64,
    t: NaiveDateTime,
    video: bool,
    favorite: bool,
    megapixels: f64,
    lat: Option<f64>,
    lon: Option<f64>,
    place: Option<i64>,
    phash: Option<u64>,
    /// Named, not hidden people in it.
    people: u8,
    junk: bool,
}

impl Cand {
    fn day(&self) -> NaiveDate {
        self.t.date()
    }

    fn score(&self) -> f64 {
        let mut s = 0.0;
        if self.favorite {
            s += 3.0;
        }
        s += f64::from(self.people.min(3));
        if !self.video {
            s += 0.5;
        }
        if self.megapixels >= 2.0 {
            s += 0.5;
        }
        s
    }
}

/// Screenshots, screen recordings, documents and pictures sent in chats, by their names.
fn junk_by_name(path: &str, mime: &str, camera: bool) -> bool {
    let p = path.to_lowercase().replace('\\', "/");
    p.contains("screenshot")
        || p.contains("screen recording")
        || p.contains("screen_recording")
        || p.contains("screenrecord")
        || (p.contains("whatsapp") && p.contains("/sent/"))
        || (!camera && matches!(mime, "image/png" | "image/gif" | "image/webp"))
}

/// A memory before it is stored.
#[derive(Debug, Clone)]
struct Built {
    key: String,
    kind: Kind,
    title: String,
    start: NaiveDate,
    end: NaiveDate,
    items: Vec<i64>,
    cover: Option<i64>,
    score: f64,
}

/// Fold near-duplicates (bursts), keeping the best of each.
fn fold_similar(mut items: Vec<&Cand>) -> Vec<&Cand> {
    items.sort_by_key(|c| (c.t, c.id));
    let mut kept: Vec<&Cand> = Vec::with_capacity(items.len());
    'next: for c in items {
        for k in kept.iter_mut().rev() {
            if (c.t - k.t).num_seconds() > SIMILAR_SECS {
                break;
            }
            if let (Some(a), Some(b)) = (c.phash, k.phash)
                && hamming(a, b) <= SIMILAR_BITS
            {
                if c.score() > k.score() {
                    *k = c;
                }
                continue 'next;
            }
        }
        kept.push(c);
    }
    kept
}

/// Pick up to `max` items spread over the time the candidates cover: the candidates (in time
/// order) are cut into `max` slices of equal count and the best item of each is taken. Videos
/// at most a quarter. Returns the items in time order and the cover (best photo).
fn curate(cands: &[&Cand], max: usize) -> (Vec<i64>, Option<i64>) {
    let items = fold_similar(cands.iter().copied().filter(|c| !c.junk).collect());
    let max_videos = max.div_ceil(4);
    let picked: Vec<&Cand> = if items.len() <= max {
        items.clone()
    } else {
        let mut videos = 0;
        let mut out = Vec::with_capacity(max);
        for i in 0..max {
            let slice = &items[i * items.len() / max..(i + 1) * items.len() / max];
            let best = |allow_video: bool| {
                slice
                    .iter()
                    .filter(|c| allow_video || !c.video)
                    .max_by(|a, b| a.score().total_cmp(&b.score()).then(b.t.cmp(&a.t)))
                    .copied()
            };
            let pick = best(videos < max_videos).or_else(|| best(true));
            if let Some(c) = pick {
                videos += usize::from(c.video);
                out.push(c);
            }
        }
        out
    };
    let cover = picked
        .iter()
        .filter(|c| !c.video)
        .max_by(|a, b| a.score().total_cmp(&b.score()).then(b.t.cmp(&a.t)))
        .or_else(|| picked.first())
        .map(|c| c.id);
    (picked.iter().map(|c| c.id).collect(), cover)
}

fn years_ago(n: i32) -> String {
    if n == 1 { "1 year ago".into() } else { format!("{n} years ago") }
}

/// Everything the rules need, read in one go.
struct Library {
    cands: Vec<Cand>,
    /// place id -> (city, admin1)
    places: HashMap<i64, (String, String)>,
    /// person (uid, name) -> their items
    people: Vec<(String, String, Vec<usize>)>,
}

fn load(db: &Db, junk: &HashSet<i64>) -> Result<Library> {
    let conn = db.conn()?;
    let mut named: HashMap<i64, u8> = HashMap::new();
    let mut people: HashMap<i64, (String, String, Vec<i64>)> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT f.media_id, p.id, p.uid, p.name FROM faces f JOIN persons p ON p.id = f.person_id
             WHERE p.name IS NOT NULL AND trim(p.name) != '' AND p.hidden = 0 AND f.locked != 2",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))?;
        for row in rows {
            let (media, person, uid, name) = row?;
            *named.entry(media).or_default() += 1;
            people.entry(person).or_insert_with(|| (uid, name, Vec::new())).2.push(media);
        }
    }

    let mut stmt = conn.prepare(&format!(
        "SELECT id, taken_at, kind, favorite, width, height, lat, lon, place_id, phash, path, mime,
                coalesce(camera_make, '') || coalesce(camera_model, '') != ''
         FROM media WHERE {VISIBLE} AND stage >= ?1"
    ))?;
    let rows = stmt.query_map([Stage::HASH], |r| {
        let (w, h): (Option<f64>, Option<f64>) = (r.get(4)?, r.get(5)?);
        let path: String = r.get(10)?;
        let mime: String = r.get(11)?;
        let camera: bool = r.get(12)?;
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)? == "video",
            r.get::<_, bool>(3)?,
            w.unwrap_or(0.0) * h.unwrap_or(0.0) / 1e6,
            (r.get::<_, Option<f64>>(6)?, r.get::<_, Option<f64>>(7)?, r.get::<_, Option<i64>>(8)?),
            r.get::<_, Option<i64>>(9)?,
            junk_by_name(&path, &mime, camera),
        ))
    })?;
    let mut cands = Vec::new();
    for row in rows {
        let (id, taken, video, favorite, megapixels, (lat, lon, place), phash, by_name) = row?;
        let Ok(t) = NaiveDateTime::parse_from_str(&taken, "%Y-%m-%dT%H:%M:%S") else { continue };
        cands.push(Cand {
            id,
            t,
            video,
            favorite,
            megapixels,
            lat,
            lon,
            place,
            phash: phash.map(|p| p as u64),
            people: named.get(&id).copied().unwrap_or(0),
            junk: by_name || junk.contains(&id),
        });
    }
    cands.sort_by_key(|c| (c.t, c.id));
    let index: HashMap<i64, usize> = cands.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
    let mut people: Vec<(String, String, Vec<usize>)> = people
        .into_values()
        .map(|(uid, name, media)| {
            let mut at: Vec<usize> = media.iter().filter_map(|m| index.get(m).copied()).collect();
            at.sort_unstable();
            (uid, name, at)
        })
        .collect();
    people.sort_by(|a, b| a.0.cmp(&b.0));

    let mut places = HashMap::new();
    let mut stmt = conn.prepare("SELECT id, coalesce(city, ''), coalesce(admin1, '') FROM places")?;
    for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))? {
        let (id, city, admin1) = row?;
        places.insert(id, (city, admin1));
    }
    Ok(Library { cands, places, people })
}

fn on_this_day(lib: &Library, today: NaiveDate) -> Vec<Built> {
    let mut out = Vec::new();
    let first_year = lib.cands.first().map_or(today.year(), |c| c.t.year());
    for year in (first_year..today.year()).rev() {
        // 29 February in a year without one: the 28th.
        let Some(anchor) = NaiveDate::from_ymd_opt(year, today.month(), today.day())
            .or_else(|| NaiveDate::from_ymd_opt(year, today.month(), today.day() - 1))
        else {
            continue;
        };
        let (from, to) = (anchor - Duration::days(DAY_WINDOW), anchor + Duration::days(DAY_WINDOW));
        let cands: Vec<&Cand> = lib.cands.iter().filter(|c| (from..=to).contains(&c.day())).collect();
        let (items, cover) = curate(&cands, DAY_MAX);
        if items.len() < DAY_MIN {
            continue;
        }
        let days: Vec<NaiveDate> = cands.iter().filter(|c| items.contains(&c.id)).map(|c| c.day()).collect();
        out.push(Built {
            key: format!("day:{anchor}"),
            kind: Kind::Day,
            title: years_ago(today.year() - year),
            start: *days.iter().min().unwrap(),
            end: *days.iter().max().unwrap(),
            items,
            cover,
            score: 0.0,
        });
    }
    out
}

/// Home: the place with the most days of photos, and the middle of its photos' positions.
fn home(lib: &Library) -> Option<(f64, f64)> {
    let mut days: HashMap<i64, HashSet<NaiveDate>> = HashMap::new();
    for c in &lib.cands {
        if let Some(p) = c.place {
            days.entry(p).or_default().insert(c.day());
        }
    }
    let (&place, _) = days.iter().max_by_key(|(id, d)| (d.len(), std::cmp::Reverse(**id)))?;
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    let at: Vec<&Cand> = lib.cands.iter().filter(|c| c.place == Some(place) && c.lat.is_some() && c.lon.is_some()).collect();
    if at.is_empty() {
        return None;
    }
    Some((median(at.iter().map(|c| c.lat.unwrap()).collect()), median(at.iter().map(|c| c.lon.unwrap()).collect())))
}

#[derive(Clone, Copy, PartialEq)]
enum Where {
    Home,
    Away,
    Unknown,
}

fn trips(lib: &Library) -> Vec<Built> {
    let Some((hlat, hlon)) = home(lib) else { return Vec::new() };
    // Items per day, and where the day was spent.
    let mut days: Vec<(NaiveDate, Vec<&Cand>)> = Vec::new();
    for c in &lib.cands {
        match days.last_mut() {
            Some((d, v)) if *d == c.day() => v.push(c),
            _ => days.push((c.day(), vec![c])),
        }
    }
    let place_of = |items: &[&Cand]| -> Where {
        let (mut away, mut known) = (0, 0);
        for c in items {
            if let (Some(lat), Some(lon)) = (c.lat, c.lon) {
                known += 1;
                if haversine_km(hlat, hlon, lat, lon) > TRIP_KM {
                    away += 1;
                }
            }
        }
        match known {
            0 => Where::Unknown,
            _ if away * 2 > known => Where::Away,
            _ => Where::Home,
        }
    };

    let mut out = Vec::new();
    let mut i = 0;
    while i < days.len() {
        if place_of(&days[i].1) != Where::Away {
            i += 1;
            continue;
        }
        // Extend over away days and days without GPS, one empty day between them allowed.
        let start = i;
        let mut last_away = i;
        let mut j = i + 1;
        while j < days.len() && (days[j].0 - days[j - 1].0).num_days() <= 2 {
            match place_of(&days[j].1) {
                Where::Home => break,
                Where::Away => last_away = j,
                Where::Unknown => {}
            }
            j += 1;
        }
        let trip = &days[start..=last_away];
        i = last_away + 1;
        let cands: Vec<&Cand> = trip.iter().flat_map(|(_, v)| v.iter().copied()).filter(|c| !c.junk).collect();
        let one_day = trip.len() == 1;
        if (one_day && cands.len() < TRIP_ONE_DAY_MIN) || cands.len() < TRIP_MIN_ITEMS {
            continue;
        }
        let (items, cover) = curate(&cands, TRIP_MAX);
        out.push(Built {
            key: format!("trip:{}", trip[0].0),
            kind: Kind::Trip,
            title: trip_title(lib, &cands, (hlat, hlon)),
            start: trip[0].0,
            end: trip[trip.len() - 1].0,
            items,
            cover,
            score: 1.0,
        });
    }
    out
}

/// Names by how often they come up, most first.
fn ranked(m: HashMap<&str, usize>) -> Vec<(&str, usize)> {
    let mut v: Vec<(&str, usize)> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    v
}

/// "Trip to Gangtok", "Trip to Gangtok and Darjeeling", else the state.
fn trip_title(lib: &Library, cands: &[&Cand], home: (f64, f64)) -> String {
    let mut cities: HashMap<&str, usize> = HashMap::new();
    let mut states: HashMap<&str, usize> = HashMap::new();
    for c in cands {
        let away = matches!((c.lat, c.lon), (Some(lat), Some(lon)) if haversine_km(home.0, home.1, lat, lon) > TRIP_KM);
        if let (true, Some((city, admin1))) = (away, c.place.and_then(|p| lib.places.get(&p))) {
            if !city.is_empty() {
                *cities.entry(city).or_default() += 1;
            }
            if !admin1.is_empty() {
                *states.entry(admin1).or_default() += 1;
            }
        }
    }
    let cities = ranked(cities);
    match cities.as_slice() {
        [] => match ranked(states).first() {
            Some((state, _)) => format!("Trip to {state}"),
            None => "A trip".into(),
        },
        [(a, _)] => format!("Trip to {a}"),
        [(a, na), (b, nb), ..] if *nb * 4 >= *na => format!("Trip to {a} and {b}"),
        [(a, _), ..] => format!("Trip to {a}"),
    }
}

fn person_memories(lib: &Library, today: NaiveDate) -> Vec<Built> {
    let mut out = Vec::new();
    for (uid, name, at) in &lib.people {
        let cands: Vec<&Cand> = at.iter().map(|&i| &lib.cands[i]).filter(|c| !c.junk).collect();
        let years: HashSet<i32> = cands.iter().map(|c| c.t.year()).collect();
        if cands.len() >= PERSON_MIN && years.len() >= 2 {
            let (items, cover) = curate(&cands, PERSON_MAX);
            out.push(Built {
                key: format!("person:{uid}"),
                kind: Kind::Person,
                title: format!("{name} over the years"),
                start: cands.first().unwrap().day(),
                end: cands.last().unwrap().day(),
                items,
                cover,
                score: 0.8,
            });
        }
        let last_year = today.year() - 1;
        let in_year: Vec<&Cand> = cands.iter().copied().filter(|c| c.t.year() == last_year).collect();
        if in_year.len() >= PERSON_YEAR_MIN {
            let (items, cover) = curate(&in_year, PERSON_MAX);
            out.push(Built {
                key: format!("person:{uid}:{last_year}"),
                kind: Kind::Person,
                title: format!("{name} in {last_year}"),
                start: in_year.first().unwrap().day(),
                end: in_year.last().unwrap().day(),
                items,
                cover,
                score: 0.7,
            });
        }
    }
    out
}

fn best_of(lib: &Library, today: NaiveDate) -> Vec<Built> {
    let mut out = Vec::new();
    let mut by_year: HashMap<i32, Vec<&Cand>> = HashMap::new();
    for c in lib.cands.iter().filter(|c| !c.junk) {
        by_year.entry(c.t.year()).or_default().push(c);
    }
    for (year, cands) in by_year.into_iter().filter(|(y, c)| *y < today.year() && c.len() >= YEAR_MIN) {
        let (items, cover) = curate(&cands, YEAR_MAX);
        out.push(Built {
            key: format!("year:{year}"),
            kind: Kind::Year,
            title: format!("Best of {year}"),
            start: NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(year, 12, 31).unwrap(),
            items,
            cover,
            score: 0.6,
        });
    }
    let mut month = today.with_day(1).unwrap();
    for _ in 0..MONTHS_BACK {
        month = (month - Duration::days(1)).with_day(1).unwrap();
        let next = (month + Duration::days(32)).with_day(1).unwrap();
        let cands: Vec<&Cand> = lib.cands.iter().filter(|c| !c.junk && c.day() >= month && c.day() < next).collect();
        if cands.len() < MONTH_MIN {
            continue;
        }
        let (items, cover) = curate(&cands, MONTH_MAX);
        let name = MONTH_NAMES[month.month0() as usize];
        let title = if month.year() == today.year() { format!("{name} highlights") } else { format!("{name} {} highlights", month.year()) };
        out.push(Built {
            key: format!("month:{}", month.format("%Y-%m")),
            kind: Kind::Month,
            title,
            start: month,
            end: next - Duration::days(1),
            items,
            cover,
            score: 0.5,
        });
    }
    out
}

/// What [`rebuild`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rebuilt {
    pub memories: usize,
    pub added: usize,
    pub removed: usize,
}

/// Work out every memory for `today` and store them: new ones are added, changed ones updated
/// in place (seen and hidden kept), ones that no longer qualify dropped (hidden ones are kept,
/// inactive, so they stay hidden if they come back). `junk` are more items to leave out
/// (e.g. pictures recognised as screenshots or documents).
pub fn rebuild(db: &Db, today: NaiveDate, junk: &HashSet<i64>) -> Result<Rebuilt> {
    let lib = load(db, junk)?;
    let removed_by_user = removed(db)?;
    let mut built = Vec::new();
    built.extend(on_this_day(&lib, today));
    built.extend(trips(&lib));
    built.extend(person_memories(&lib, today));
    built.extend(best_of(&lib, today));

    // Photos the user took out of a memory: rebuild that one without them.
    let built: Vec<Built> = built
        .into_iter()
        .filter_map(|mut b| {
            if let Some(gone) = removed_by_user.get(&b.key) {
                b.items.retain(|id| !gone.contains(id));
                if b.cover.is_some_and(|c| gone.contains(&c)) {
                    b.cover = b.items.first().copied();
                }
            }
            (b.items.len() >= DAY_MIN).then_some(b)
        })
        .collect();

    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let existing: HashSet<String> = {
        let mut stmt = tx.prepare("SELECT key FROM memories")?;
        stmt.query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?
    };
    let mut report = Rebuilt { memories: built.len(), ..Rebuilt::default() };
    let keys: HashSet<&str> = built.iter().map(|b| b.key.as_str()).collect();
    for b in &built {
        if !existing.contains(&b.key) {
            report.added += 1;
        }
        tx.prepare_cached(
            "INSERT INTO memories (key, kind, title, start_day, end_day, cover_id, items, score, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
             ON CONFLICT (key) DO UPDATE SET kind = excluded.kind, title = excluded.title,
                 start_day = excluded.start_day, end_day = excluded.end_day, cover_id = excluded.cover_id,
                 items = excluded.items, score = excluded.score, active = 1,
                 updated_at = CASE WHEN memories.items = excluded.items AND memories.title = excluded.title
                                   THEN memories.updated_at ELSE excluded.updated_at END",
        )?
        .execute(params![
            b.key,
            b.kind.as_str(),
            b.title,
            b.start.to_string(),
            b.end.to_string(),
            b.cover,
            serde_json::to_string(&b.items)?,
            b.score,
            now
        ])?;
    }
    for key in existing.iter().filter(|k| !keys.contains(k.as_str())) {
        let n = tx.execute("DELETE FROM memories WHERE key = ?1 AND hidden = 0", [key])?;
        if n == 0 {
            tx.execute("UPDATE memories SET active = 0 WHERE key = ?1", [key])?;
        }
        report.removed += 1;
    }
    tx.commit()?;
    Ok(report)
}

fn removed(db: &Db) -> Result<HashMap<String, HashSet<i64>>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare("SELECT key, media_id FROM memory_removed")?;
    let mut out: HashMap<String, HashSet<i64>> = HashMap::new();
    for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (key, id) = row?;
        out.entry(key).or_default().insert(id);
    }
    Ok(out)
}

/// A memory as the UI lists it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub id: i64,
    pub kind: Kind,
    pub title: String,
    /// First and last day, `YYYY-MM-DD`.
    pub start: String,
    pub end: String,
    pub cover_id: Option<i64>,
    /// Modification time (seconds) of the cover's file, for the thumbnail URL.
    pub cover_m: Option<i64>,
    /// Items still visible.
    pub count: usize,
    pub seen: bool,
    pub hidden: bool,
}

/// A memory and its items, in order.
#[derive(Debug, Clone, Serialize)]
pub struct Memory {
    #[serde(flatten)]
    pub card: Card,
    pub items: Vec<TimelineItem>,
}

/// Visible items of a memory, in its order.
fn visible_items(conn: &r2d2_sqlite::rusqlite::Connection, json: &str) -> Result<Vec<TimelineItem>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT m.id, coalesce(m.edit_width, m.width), coalesce(m.edit_height, m.height), m.taken_at, m.kind, m.duration_ms,
             max(m.mtime, coalesce(m.edited_at, 0)), m.favorite
         FROM json_each(?1) j JOIN media m ON m.id = j.value
         WHERE {VISIBLE_M} AND m.stage >= ?2 ORDER BY j.key"
    ))?;
    let items = stmt
        .query_map(params![json, Stage::METADATA], |r| {
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

const CARD_COLUMNS: &str = "id, kind, title, start_day, end_day, cover_id, items, seen_at IS NOT NULL, hidden";

fn card_row(conn: &r2d2_sqlite::rusqlite::Connection, r: &r2d2_sqlite::rusqlite::Row) -> Result<(Card, Vec<TimelineItem>)> {
    let items = visible_items(conn, &r.get::<_, String>(6)?)?;
    let cover: Option<i64> = r.get(5)?;
    let cover = cover.filter(|c| items.iter().any(|i| i.id == *c)).or_else(|| items.iter().find(|i| !i.v).map(|i| i.id));
    let card = Card {
        id: r.get(0)?,
        kind: Kind::parse(&r.get::<_, String>(1)?),
        title: r.get(2)?,
        start: r.get(3)?,
        end: r.get(4)?,
        cover_id: cover,
        cover_m: cover.and_then(|c| items.iter().find(|i| i.id == c)).map(|i| i.m),
        count: items.len(),
        seen: r.get(7)?,
        hidden: r.get(8)?,
    };
    Ok((card, items))
}

/// Memories to show, in order: today's "on this day" (most recent year first), then the rest,
/// unseen first, newest first. Hidden ones only if asked. Memories left with fewer than 3
/// visible items (archived or moved to the locked folder since) are left out.
pub fn list(db: &Db, include_hidden: bool) -> Result<Vec<Card>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {CARD_COLUMNS} FROM memories WHERE active = 1 AND (?1 OR hidden = 0)
         ORDER BY kind != 'day', CASE WHEN kind = 'day' THEN 0 ELSE seen_at IS NOT NULL END,
                  end_day DESC, score DESC, id"
    ))?;
    let mut rows = stmt.query([include_hidden])?;
    let mut out = Vec::new();
    while let Some(r) = rows.next()? {
        let (card, _) = card_row(&conn, r)?;
        if card.count >= DAY_MIN {
            out.push(card);
        }
    }
    Ok(out)
}

pub fn get(db: &Db, id: i64) -> Result<Memory> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare(&format!("SELECT {CARD_COLUMNS} FROM memories WHERE id = ?1"))?;
    let mut rows = stmt.query([id])?;
    let r = rows.next()?.ok_or(Error::NotFound)?;
    let (card, items) = card_row(&conn, r)?;
    Ok(Memory { card, items })
}

pub fn mark_seen(db: &Db, id: i64) -> Result<()> {
    let now = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let n = db.conn()?.execute("UPDATE memories SET seen_at = coalesce(seen_at, ?2) WHERE id = ?1", params![id, now])?;
    if n == 0 { Err(Error::NotFound) } else { Ok(()) }
}

pub fn set_hidden(db: &Db, id: i64, hidden: bool) -> Result<()> {
    let n = db.conn()?.execute("UPDATE memories SET hidden = ?2 WHERE id = ?1", params![id, hidden])?;
    if n == 0 { Err(Error::NotFound) } else { Ok(()) }
}

/// Take items out of a memory; they stay out when it is rebuilt.
pub fn remove_items(db: &Db, id: i64, media_ids: &[i64]) -> Result<Memory> {
    {
        let mut conn = db.conn()?;
        let tx = conn.transaction()?;
        let (key, items): (String, String) = tx
            .query_row("SELECT key, items FROM memories WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .ok_or(Error::NotFound)?;
        let mut list: Vec<i64> = serde_json::from_str(&items)?;
        list.retain(|i| !media_ids.contains(i));
        for m in media_ids {
            tx.execute(
                "INSERT OR IGNORE INTO memory_removed (key, media_id) SELECT ?1, id FROM media WHERE id = ?2",
                params![key, m],
            )?;
        }
        tx.execute(
            "UPDATE memories SET items = ?2,
                 cover_id = CASE WHEN cover_id IN (SELECT value FROM json_each(?3)) THEN NULL ELSE cover_id END
             WHERE id = ?1",
            params![id, serde_json::to_string(&list)?, serde_json::to_string(media_ids)?],
        )?;
        tx.commit()?;
    }
    get(db, id)
}

#[cfg(test)]
mod tests;
