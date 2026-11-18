use std::path::Path;

use super::*;
use crate::db::test_db;

const KOLKATA: (f64, f64) = (22.57, 88.36);
const GANGTOK: (f64, f64) = (27.33, 88.61);

struct Lib {
    _dir: tempfile::TempDir,
    db: Db,
    folder: i64,
    n: u64,
}

#[derive(Default, Clone)]
struct Opts {
    gps: Option<(f64, f64)>,
    place: Option<i64>,
    video: bool,
    /// Same picture as another (a burst): shares its perceptual hash.
    phash: Option<u64>,
    favorite: bool,
    name: Option<&'static str>,
}

fn lib() -> Lib {
    let (dir, db) = test_db();
    let folder = crate::folders::add(&db, dir.path()).unwrap().id;
    Lib { _dir: dir, db, folder, n: 0 }
}

impl Lib {
    fn place(&self, city: &str, admin1: &str) -> i64 {
        let conn = self.db.conn().unwrap();
        conn.execute("INSERT INTO places (country, admin1, city) VALUES ('IN', ?1, ?2)", params![admin1, city]).unwrap();
        conn.last_insert_rowid()
    }

    fn add(&mut self, taken: &str, o: Opts) -> i64 {
        self.n += 1;
        // Spread-out hashes, so items are not near-duplicates unless asked.
        let phash = o.phash.unwrap_or(self.n.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let name = o.name.map(str::to_owned).unwrap_or_else(|| format!("IMG_{}.jpg", self.n));
        let path = Path::new("C:\\Photos").join(&name);
        let conn = self.db.conn().unwrap();
        conn.execute(
            "INSERT INTO media (folder_id, path, name, size, mtime, kind, mime, width, height, taken_at, lat, lon,
                                place_id, phash, favorite, stage, added_at)
             VALUES (?1, ?2, ?3, 1, 1000, ?4, ?5, 4000, 3000, ?6, ?7, ?8, ?9, ?10, ?11, 3, '')",
            params![
                self.folder,
                path.to_string_lossy(),
                name,
                if o.video { "video" } else { "photo" },
                if o.video { "video/mp4" } else if name.ends_with(".png") { "image/png" } else { "image/jpeg" },
                taken,
                o.gps.map(|g| g.0),
                o.gps.map(|g| g.1),
                o.place,
                phash as i64,
                o.favorite,
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn rebuild(&self, today: &str) -> Rebuilt {
        rebuild(&self.db, NaiveDate::parse_from_str(today, "%Y-%m-%d").unwrap(), &HashSet::new()).unwrap()
    }

    fn by_key(&self, key: &str) -> Option<Memory> {
        let id: Option<i64> =
            self.db.conn().unwrap().query_row("SELECT id FROM memories WHERE key = ?1", [key], |r| r.get(0)).optional().unwrap();
        id.map(|id| get(&self.db, id).unwrap())
    }

    fn keys(&self) -> Vec<String> {
        let mut k: Vec<String> = {
            let conn = self.db.conn().unwrap();
            let mut stmt = conn.prepare("SELECT key FROM memories WHERE active = 1").unwrap();
            stmt.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
        };
        k.sort();
        k
    }
}

/// A year of everyday photos at home: one or two a week, too few for "best of".
fn home_life(l: &mut Lib, year: i32, home: i64) {
    for week in 0..30 {
        let day = NaiveDate::from_ymd_opt(year, 1, 5).unwrap() + Duration::days(week * 7);
        l.add(&format!("{day}T18:00:00"), Opts { gps: Some(KOLKATA), place: Some(home), ..Opts::default() });
    }
}

#[test]
fn on_this_day_folds_bursts_and_skips_screenshots() {
    let mut l = lib();
    // 3 years ago: four photos within the window.
    for (i, day) in ["2023-11-16", "2023-11-17", "2023-11-18", "2023-11-21"].iter().enumerate() {
        l.add(&format!("{day}T1{i}:00:00"), Opts::default());
    }
    // Outside the ±3 days.
    l.add("2023-11-25T10:00:00", Opts::default());
    // 4 years ago: a burst of the same picture and two screenshots: only one real photo.
    for s in 0..3 {
        l.add(&format!("2022-11-18T10:00:0{s}"), Opts { phash: Some(42), ..Opts::default() });
    }
    l.add("2022-11-18T11:00:00", Opts { name: Some("Screenshot_20221118.jpg"), ..Opts::default() });
    l.add("2022-11-18T11:00:01", Opts { name: Some("chart.png"), ..Opts::default() });
    // This year doesn't count.
    for h in 0..4 {
        l.add(&format!("2026-11-18T0{h}:00:00"), Opts::default());
    }

    l.rebuild("2026-11-18");
    let m = l.by_key("day:2023-11-18").expect("3 years ago");
    assert_eq!(m.card.title, "3 years ago");
    assert_eq!(m.items.len(), 4);
    assert_eq!((m.card.start.as_str(), m.card.end.as_str()), ("2023-11-16", "2023-11-21"));
    assert!(l.by_key("day:2022-11-18").is_none(), "{:?}", l.keys());

    // The next day it moves on: yesterday's memory is replaced by today's.
    l.rebuild("2026-11-19");
    assert!(l.by_key("day:2023-11-18").is_none());
    assert_eq!(l.by_key("day:2023-11-19").unwrap().items.len(), 4);
    // A week later nothing qualifies.
    l.rebuild("2026-11-28");
    assert!(l.keys().iter().all(|k| !k.starts_with("day:")), "{:?}", l.keys());
}

#[test]
fn trips_far_from_home() {
    let mut l = lib();
    let home = l.place("Kolkata", "West Bengal");
    let gangtok = l.place("Gangtok", "Sikkim");
    let darjeeling = l.place("Darjeeling", "West Bengal");
    home_life(&mut l, 2023, home);
    // 2-5 October: Gangtok, a day without GPS, then Darjeeling (an empty day before it).
    for h in 0..4 {
        l.add(&format!("2023-10-02T1{h}:00:00"), Opts { gps: Some(GANGTOK), place: Some(gangtok), ..Opts::default() });
        l.add(&format!("2023-10-03T1{h}:00:00"), Opts { gps: Some(GANGTOK), place: Some(gangtok), ..Opts::default() });
    }
    l.add("2023-10-04T12:00:00", Opts::default());
    for h in 0..3 {
        l.add(&format!("2023-10-06T1{h}:00:00"), Opts { gps: Some((27.04, 88.26)), place: Some(darjeeling), ..Opts::default() });
    }
    // Back home on the 7th; a day trip with only 3 photos later is not a trip.
    l.add("2023-10-07T10:00:00", Opts { gps: Some(KOLKATA), place: Some(home), ..Opts::default() });
    for h in 0..3 {
        l.add(&format!("2023-12-01T1{h}:00:00"), Opts { gps: Some(GANGTOK), place: Some(gangtok), ..Opts::default() });
    }

    l.rebuild("2026-11-18");
    let trip = l.by_key("trip:2023-10-02").unwrap_or_else(|| panic!("{:?}", l.keys()));
    assert_eq!(trip.card.title, "Trip to Gangtok and Darjeeling");
    assert_eq!(trip.card.kind, Kind::Trip);
    assert_eq!((trip.card.start.as_str(), trip.card.end.as_str()), ("2023-10-02", "2023-10-06"));
    assert_eq!(trip.items.len(), 12, "the day without GPS counts");
    assert!(l.by_key("trip:2023-12-01").is_none());
    assert_eq!(l.keys().iter().filter(|k| k.starts_with("trip:")).count(), 1, "{:?}", l.keys());
}

fn name_person(l: &Lib, name: &str, media: &[i64]) {
    let conn = l.db.conn().unwrap();
    conn.execute("INSERT INTO persons (name, created_at) VALUES (?1, '')", [name]).unwrap();
    let person = conn.last_insert_rowid();
    for m in media {
        conn.execute(
            "INSERT INTO faces (media_id, x, y, w, h, score, embedding, person_id) VALUES (?1, 0.1, 0.1, 0.2, 0.2, 0.9, x'00', ?2)",
            params![m, person],
        )
        .unwrap();
    }
}

#[test]
fn people_and_best_of() {
    let mut l = lib();
    let mut asha = Vec::new();
    for i in 0..8 {
        asha.push(l.add(&format!("2024-0{}-10T10:00:00", i + 1), Opts::default()));
    }
    for i in 0..16 {
        asha.push(l.add(&format!("2025-{:02}-{:02}T10:00:00", i % 12 + 1, i / 12 + 3), Opts::default()));
    }
    name_person(&l, "Asha", &asha);
    // A favorite in 2025 should be in the best of.
    let fav = l.add("2025-06-15T10:00:00", Opts { favorite: true, ..Opts::default() });
    for i in 0..60 {
        l.add(&format!("2025-{:02}-{:02}T1{}:00:00", i % 12 + 1, i % 28 + 1, i % 10), Opts::default());
    }
    // October 2026 highlights: 30 photos and 2 videos.
    for i in 0..30 {
        l.add(&format!("2026-10-{:02}T1{}:00:00", i % 28 + 1, i % 10), Opts::default());
    }
    l.add("2026-10-05T09:00:00", Opts { video: true, ..Opts::default() });

    l.rebuild("2026-11-18");
    let over = l.by_key(&format!("person:{}", person_uid(&l, "Asha"))).unwrap();
    assert_eq!(over.card.title, "Asha over the years");
    assert_eq!(over.items.len(), 24);
    let year = l.by_key(&format!("person:{}:2025", person_uid(&l, "Asha"))).unwrap();
    assert_eq!(year.card.title, "Asha in 2025");
    assert_eq!(year.items.len(), 16);

    let best = l.by_key("year:2025").unwrap();
    assert_eq!(best.card.title, "Best of 2025");
    assert_eq!(best.items.len(), YEAR_MAX);
    assert!(best.items.iter().any(|i| i.id == fav), "favorites are picked");
    assert!(best.items.windows(2).all(|w| w[0].t <= w[1].t), "in time order");
    assert!(l.by_key("year:2024").is_none(), "too few");
    assert!(l.by_key("year:2026").is_none(), "this year isn't over");
    let month = l.by_key("month:2026-10").unwrap();
    assert_eq!((month.card.title.as_str(), month.items.len()), ("October highlights", 31));
    assert!(month.card.cover_id.is_some_and(|c| month.items.iter().any(|i| i.id == c && !i.v)), "cover is a photo");
}

fn person_uid(l: &Lib, name: &str) -> String {
    l.db.conn().unwrap().query_row("SELECT uid FROM persons WHERE name = ?1", [name], |r| r.get(0)).unwrap()
}

#[test]
fn seen_hidden_removed_and_archived_survive_rebuilds() {
    let mut l = lib();
    let ids: Vec<i64> = (0..6).map(|h| l.add(&format!("2025-11-18T1{h}:00:00"), Opts::default())).collect();
    l.rebuild("2026-11-18");
    let m = l.by_key("day:2025-11-18").unwrap();
    assert_eq!((m.card.title.as_str(), m.items.len(), m.card.seen), ("1 year ago", 6, false));
    let cards = list(&l.db, false).unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].count, 6);

    mark_seen(&l.db, m.card.id).unwrap();
    let after = remove_items(&l.db, m.card.id, &[ids[0]]).unwrap();
    assert_eq!(after.items.len(), 5);
    crate::organize::archive(&l.db, &[ids[1]]).unwrap();
    assert_eq!(get(&l.db, m.card.id).unwrap().items.len(), 4, "archived items leave at once");

    let r = l.rebuild("2026-11-18");
    assert_eq!(r.added, 0);
    let m2 = l.by_key("day:2025-11-18").unwrap();
    assert_eq!(m2.card.id, m.card.id, "same memory, updated in place");
    assert!(m2.card.seen);
    assert_eq!(m2.items.len(), 4, "removed and archived stay out");

    set_hidden(&l.db, m.card.id, true).unwrap();
    assert!(list(&l.db, false).unwrap().is_empty());
    assert_eq!(list(&l.db, true).unwrap().len(), 1);
    // Gone the next day but kept (inactive) because it is hidden; back hidden on the date.
    l.rebuild("2026-11-28");
    assert!(list(&l.db, true).unwrap().is_empty());
    l.rebuild("2026-11-18");
    let back = l.by_key("day:2025-11-18").unwrap();
    assert!(back.card.hidden);

    // Fewer than 3 items left: not listed.
    crate::organize::archive(&l.db, &ids[2..5]).unwrap();
    assert!(list(&l.db, true).unwrap().is_empty());
}

#[test]
fn curate_spreads_and_caps_videos() {
    let cands: Vec<Cand> = (0..100)
        .map(|i| Cand {
            id: i,
            t: NaiveDate::from_ymd_opt(2025, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap() + Duration::hours(i * 5),
            video: i % 2 == 0,
            favorite: false,
            megapixels: 12.0,
            lat: None,
            lon: None,
            place: None,
            phash: Some((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
            people: 0,
            junk: false,
        })
        .collect();
    let refs: Vec<&Cand> = cands.iter().collect();
    let (items, cover) = curate(&refs, 20);
    assert_eq!(items.len(), 20);
    let videos = items.iter().filter(|&&i| i % 2 == 0).count();
    assert!(videos <= 5, "{videos} videos");
    assert!(items.first().unwrap() < &10 && items.last().unwrap() > &90, "spread over the whole time: {items:?}");
    assert!(cover.is_some_and(|c| c % 2 == 1), "cover is a photo");
}
