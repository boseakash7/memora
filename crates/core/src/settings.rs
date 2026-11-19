use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::db::Db;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    System,
    Light,
    Dark,
}

/// How much the interface animates. `Auto` picks `High` on a PC with a graphics card and
/// `Medium` otherwise (or `None` when Windows animations are turned off); the UI decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Animations {
    Auto,
    High,
    Medium,
    None,
}

/// User settings. Stored one key per row so partial updates are easy;
/// missing keys fall back to the defaults below.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Name the TV shows for this PC in its media sources list.
    pub device_name: String,
    pub theme: Theme,
    pub animations: Animations,
    /// Share the library on the home network (TV page + DLNA).
    pub lan_sharing: bool,
    pub lan_port: u16,
    /// While sharing: the web page for TV browsers and phones at `/tv`.
    pub lan_tv_page: bool,
    /// While sharing: the DLNA media server TVs find in their source list. TVs can't enter
    /// a PIN, so anyone on the network can browse it.
    pub lan_dlna: bool,
    /// While sharing: an Archive row on the TV page and an Archive folder for DLNA.
    pub lan_archive: bool,
    /// Optional PIN required to open the TV web page.
    pub tv_pin: Option<String>,
    /// Convert videos the TV can't play (HEVC, MKV, ...) on the fly.
    pub transcode_videos: bool,
    pub ml_enabled: bool,
    pub ml_use_gpu: bool,
    /// Lock the locked folder after this many minutes without using it.
    pub vault_lock_minutes: u32,
    /// Also lock it when Memora is minimized.
    pub vault_lock_on_minimize: bool,
    /// Memories ("on this day", trips...) above the timeline and on their own page.
    pub memories: bool,
    /// The first-start welcome (AI features, first folder) was finished or skipped.
    pub welcome_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let host = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
        Settings {
            device_name: format!("Memora ({host})"),
            theme: Theme::System,
            animations: Animations::Auto,
            lan_sharing: false,
            lan_port: 8200,
            lan_tv_page: true,
            lan_dlna: true,
            lan_archive: true,
            tv_pin: None,
            transcode_videos: true,
            ml_enabled: true,
            ml_use_gpu: true,
            vault_lock_minutes: 5,
            vault_lock_on_minimize: false,
            memories: true,
            welcome_done: false,
        }
    }
}

impl Settings {
    fn validate(&self) -> Result<()> {
        let name = self.device_name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(Error::invalid("device name must be 1-64 characters"));
        }
        if self.lan_port < 1024 {
            return Err(Error::invalid("network port must be 1024 or higher"));
        }
        if !(1..=60).contains(&self.vault_lock_minutes) {
            return Err(Error::invalid("lock after 1-60 minutes"));
        }
        if let Some(pin) = &self.tv_pin
            && (!(4..=8).contains(&pin.len()) || !pin.chars().all(|c| c.is_ascii_digit()))
        {
            return Err(Error::invalid("PIN must be 4-8 digits"));
        }
        Ok(())
    }
}

pub fn load(db: &Db) -> Result<Settings> {
    Ok(serde_json::from_value(Value::Object(load_raw(db)?))?)
}

/// Apply a partial update, e.g. `{"lanSharing": true}`. Unknown keys are rejected.
pub fn update(db: &Db, patch: &Value) -> Result<Settings> {
    let patch = patch
        .as_object()
        .ok_or_else(|| Error::invalid("settings update must be a JSON object"))?;

    let mut merged = match serde_json::to_value(load(db)?)? {
        Value::Object(map) => map,
        _ => unreachable!("Settings serializes to an object"),
    };
    for (key, value) in patch {
        if !merged.contains_key(key) {
            return Err(Error::invalid(format!("unknown setting '{key}'")));
        }
        merged.insert(key.clone(), value.clone());
    }
    let settings: Settings = serde_json::from_value(Value::Object(merged.clone()))
        .map_err(|e| Error::invalid(format!("invalid setting value: {e}")))?;
    settings.validate()?;

    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    for key in patch.keys() {
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, serde_json::to_string(&merged[key])?),
        )?;
    }
    tx.commit()?;
    Ok(settings)
}

/// A random value created on first use and kept for good, stored next to the settings
/// but not part of them (not shown or editable). E.g. the DLNA device id.
pub fn persistent_secret(db: &Db, key: &str) -> Result<String> {
    let key = format!("_{key}");
    let conn = db.conn()?;
    let fresh = uuid::Uuid::new_v4().simple().to_string() + &uuid::Uuid::new_v4().simple().to_string();
    conn.execute("INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)", (&key, serde_json::to_string(&fresh)?))?;
    let value: String = conn.query_row("SELECT value FROM settings WHERE key = ?1", [&key], |r| r.get(0))?;
    Ok(serde_json::from_str(&value)?)
}

fn load_raw(db: &Db) -> Result<Map<String, Value>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare("SELECT key, value FROM settings")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut map = Map::new();
    for row in rows {
        let (key, value) = row?;
        // Skip unreadable values rather than failing to start; the default is used instead.
        if key.starts_with('_') {
            continue; // persistent_secret values
        }
        if let Ok(v) = serde_json::from_str(&value) {
            map.insert(key, v);
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use serde_json::json;

    #[test]
    fn defaults_then_partial_update() {
        let (_dir, db) = test_db();
        assert_eq!(load(&db).unwrap(), Settings::default());

        let s = update(&db, &json!({"lanSharing": true, "tvPin": "1234"})).unwrap();
        assert!(s.lan_sharing);
        assert_eq!(load(&db).unwrap().tv_pin.as_deref(), Some("1234"));
        assert_eq!(load(&db).unwrap().lan_port, 8200);

        let s = update(&db, &json!({"animations": "none"})).unwrap();
        assert_eq!(s.animations, Animations::None);

        // PIN can be cleared again.
        assert_eq!(update(&db, &json!({"tvPin": null})).unwrap().tv_pin, None);
    }

    #[test]
    fn secrets_are_stable_and_hidden() {
        let (_dir, db) = test_db();
        let a = persistent_secret(&db, "dlnaUuid").unwrap();
        assert_eq!(a.len(), 64);
        assert_eq!(persistent_secret(&db, "dlnaUuid").unwrap(), a);
        assert_ne!(persistent_secret(&db, "other").unwrap(), a);
        assert_eq!(load(&db).unwrap(), Settings::default());
        assert!(update(&db, &json!({"_dlnaUuid": "x"})).is_err());
    }

    #[test]
    fn rejects_bad_values() {
        let (_dir, db) = test_db();
        assert!(update(&db, &json!({"nope": 1})).is_err());
        assert!(update(&db, &json!({"lanPort": 80})).is_err());
        assert!(update(&db, &json!({"tvPin": "12ab"})).is_err());
        assert!(update(&db, &json!({"theme": "purple"})).is_err());
        assert!(update(&db, &json!({"animations": "extreme"})).is_err());
        assert!(update(&db, &json!({"deviceName": "  "})).is_err());
        assert_eq!(load(&db).unwrap(), Settings::default());
    }
}
