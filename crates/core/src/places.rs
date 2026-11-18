//! Places: GPS coordinates turned into city names, fully offline.
//!
//! Uses the GeoNames city list built into the `reverse_geocoder` crate. A photo gets the
//! nearest city, unless none is close (open sea, polar regions).

use std::sync::OnceLock;

use r2d2_sqlite::rusqlite::{OptionalExtension, params};
use reverse_geocoder::ReverseGeocoder;
use serde::Serialize;

use crate::db::Db;
use crate::media::VISIBLE_M;
use crate::{Error, Result};

/// Photos farther than this from the nearest city get no place.
const MAX_DISTANCE_KM: f64 = 50.0;

/// Where a photo was taken. `country` is an ISO 3166-1 alpha-2 code; the UI shows its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlaceName {
    pub country: String,
    pub admin1: String,
    pub city: String,
}

fn geocoder() -> &'static ReverseGeocoder {
    // Building the search tree takes a moment, so do it once, on first use.
    static GEOCODER: OnceLock<ReverseGeocoder> = OnceLock::new();
    GEOCODER.get_or_init(ReverseGeocoder::new)
}

fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dlat = p2 - p1;
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dlon / 2.0).sin().powi(2);
    6371.0 * 2.0 * a.sqrt().asin()
}

/// The place for a coordinate, or None if it is invalid, (0, 0), or far from any city.
pub fn lookup(lat: f64, lon: f64) -> Option<PlaceName> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) || (lat == 0.0 && lon == 0.0) {
        return None;
    }
    let found = geocoder().search((lat, lon));
    let r = found.record;
    if haversine_km(lat, lon, r.lat, r.lon) > MAX_DISTANCE_KM {
        return None;
    }
    Some(PlaceName { country: r.cc.clone(), admin1: r.admin1.clone(), city: r.name.clone() })
}

/// Id of the place row, created if new.
pub(crate) fn intern(conn: &r2d2_sqlite::rusqlite::Connection, place: &PlaceName) -> Result<i64> {
    conn.execute(
        "INSERT OR IGNORE INTO places (country, admin1, city) VALUES (?1, ?2, ?3)",
        params![place.country, place.admin1, place.city],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM places WHERE country = ?1 AND admin1 = ?2 AND city = ?3",
        params![place.country, place.admin1, place.city],
        |r| r.get(0),
    )?)
}

/// A place with the photos taken there.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    pub id: i64,
    pub country: String,
    pub admin1: String,
    pub city: String,
    /// Visible items (not trashed, not missing).
    pub count: i64,
    /// Newest item there, as the cover.
    pub cover_id: Option<i64>,
    /// Modification time (seconds) of the cover's file, for the thumbnail URL.
    pub cover_m: Option<i64>,
}

fn select_places(filter: &str) -> String {
    let visible_m2 = VISIBLE_M.replace("m.", "m2.");
    format!(
        "SELECT p.id, p.country, p.admin1, p.city, x.cnt, x.cover,
                (SELECT mtime / 1000 FROM media WHERE id = x.cover)
         FROM places p JOIN (
             SELECT place_id, count(*) AS cnt,
                 (SELECT m2.id FROM media m2
                   WHERE m2.place_id = m.place_id AND {visible_m2} AND m2.stage >= 1
                   ORDER BY m2.taken_at DESC, m2.id DESC LIMIT 1) AS cover
             FROM media m
             WHERE m.place_id IS NOT NULL AND {VISIBLE_M} AND m.stage >= 1
             GROUP BY place_id
         ) x ON x.place_id = p.id
         {filter}"
    )
}

fn from_row(r: &r2d2_sqlite::rusqlite::Row) -> r2d2_sqlite::rusqlite::Result<Place> {
    Ok(Place {
        id: r.get(0)?,
        country: r.get(1)?,
        admin1: r.get(2)?,
        city: r.get(3)?,
        count: r.get(4)?,
        cover_id: r.get(5)?,
        cover_m: r.get(6)?,
    })
}

/// Places that have visible photos, by country and then by number of photos.
pub fn list(db: &Db) -> Result<Vec<Place>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare(&format!(
        "{} ORDER BY p.country, x.cnt DESC, p.city COLLATE NOCASE",
        select_places("")
    ))?;
    Ok(stmt.query_map([], from_row)?.collect::<std::result::Result<_, _>>()?)
}

pub fn get(db: &Db, id: i64) -> Result<Place> {
    let conn = db.conn()?;
    conn.query_row(&select_places("WHERE p.id = ?1"), [id], from_row)
        .optional()?
        .ok_or(Error::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_cities() {
        let paris = lookup(48.8566, 2.3522).unwrap();
        assert_eq!((paris.city.as_str(), paris.country.as_str()), ("Paris", "FR"));
        let tokyo = lookup(35.68, 139.76).unwrap();
        assert_eq!(tokyo.country, "JP");
        // A few km outside the centre still resolves.
        assert_eq!(lookup(48.90, 2.40).unwrap().country, "FR");
    }

    #[test]
    fn no_place_for_open_sea_or_bad_coordinates() {
        assert_eq!(lookup(-45.0, -130.0), None, "south Pacific");
        assert_eq!(lookup(0.0, 0.0), None, "null island");
        assert_eq!(lookup(91.0, 10.0), None);
        assert_eq!(lookup(10.0, 181.0), None);
        assert_eq!(lookup(f64::NAN, 10.0), None);
    }
}
