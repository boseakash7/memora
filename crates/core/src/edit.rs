//! Photo editing, saved non-destructively: an [`Edit`] (a small JSON recipe: crop, rotation,
//! adjustments, filter, background blur) is stored per item and applied whenever a picture
//! of it is made (grid thumbnails, the viewer's copy, the TV's), never to the file itself.
//! Reverting just removes the recipe. The recipe travels in `.memora` like favorites do.
//!
//! The renderer ([`render`]) is the only implementation: the editor's live preview asks the
//! server for small renders, so the preview and the saved result can't drift apart.

pub mod auto;
mod bokeh;
pub mod color;
pub mod filters;
pub mod render;
pub mod session;

use r2d2_sqlite::rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::{Error, Result};

/// A crop, as fractions of the picture after rotation and flipping (0..1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Crop {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Slider values, each -100..100 (Sharpen and Grain 0..100). 0 leaves the photo alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Adjust {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub temperature: f32,
    pub tint: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub sharpen: f32,
    pub vignette: f32,
    pub grain: f32,
}

impl Adjust {
    fn values_mut(&mut self) -> [&mut f32; 15] {
        [
            &mut self.exposure,
            &mut self.contrast,
            &mut self.highlights,
            &mut self.shadows,
            &mut self.whites,
            &mut self.blacks,
            &mut self.temperature,
            &mut self.tint,
            &mut self.saturation,
            &mut self.vibrance,
            &mut self.clarity,
            &mut self.dehaze,
            &mut self.sharpen,
            &mut self.vignette,
            &mut self.grain,
        ]
    }

    pub fn is_zero(&self) -> bool {
        let mut a = *self;
        a.values_mut().iter().all(|v| **v == 0.0)
    }
}

/// What the background blur looks like (see `bokeh`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlurStyle {
    /// Round bokeh, like a fast lens.
    #[default]
    Lens,
    /// Tall oval bokeh, like an anamorphic cinema lens.
    Oval,
    /// A smooth, even blur.
    Soft,
    /// Swirling around the centre, like old Petzval or Helios lenses.
    Swirl,
    /// Sideways streaks, as when panning.
    Motion,
    /// Streaks out from the subject, as when zooming during the exposure.
    Zoom,
    /// A style from a newer version: shown as `Lens`.
    #[serde(other)]
    Unknown,
}

impl BlurStyle {
    pub fn resolved(self) -> BlurStyle {
        if self == BlurStyle::Unknown { BlurStyle::Lens } else { self }
    }
}

/// How a photo is edited. Geometry is applied in this order: the file's own EXIF rotation,
/// `flip` (mirror left-right), `rotate` quarter turns clockwise, `straighten`, then `crop`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Edit {
    /// Quarter turns clockwise, 0..3.
    pub rotate: u8,
    pub flip: bool,
    /// Degrees clockwise, -45..45.
    pub straighten: f32,
    pub crop: Option<Crop>,
    pub adjust: Adjust,
    pub filter: Option<String>,
    /// How much of the filter, 0..1.
    pub filter_strength: f32,
    /// Background blur behind the main subject (found by the segmentation model), 0..100.
    pub blur: f32,
    pub blur_style: BlurStyle,
}

impl Default for Edit {
    fn default() -> Self {
        Edit {
            rotate: 0,
            flip: false,
            straighten: 0.0,
            crop: None,
            adjust: Adjust::default(),
            filter: None,
            filter_strength: 1.0,
            blur: 0.0,
            blur_style: BlurStyle::Lens,
        }
    }
}

fn finite(v: f32, lo: f32, hi: f32) -> f32 {
    if v.is_finite() { v.clamp(lo, hi) } else { 0.0 }
}

impl Edit {
    /// Bring every value into range (recipes come from the UI and from other PCs' logs).
    pub fn normalized(mut self) -> Edit {
        self.rotate %= 4;
        self.straighten = finite(self.straighten, -45.0, 45.0);
        if self.straighten.abs() < 0.01 {
            self.straighten = 0.0;
        }
        self.crop = self.crop.and_then(|c| {
            let x = finite(c.x, 0.0, 1.0);
            let y = finite(c.y, 0.0, 1.0);
            let w = finite(c.w, 0.0, 1.0 - x);
            let h = finite(c.h, 0.0, 1.0 - y);
            if w < 0.005 || h < 0.005 {
                return None;
            }
            let full = x < 1e-4 && y < 1e-4 && w > 0.9999 - x && h > 0.9999 - y;
            (!full).then_some(Crop { x, y, w, h })
        });
        let a = &mut self.adjust;
        for v in a.values_mut() {
            *v = finite(*v, -100.0, 100.0);
        }
        a.sharpen = a.sharpen.max(0.0);
        a.grain = a.grain.max(0.0);
        self.filter = self.filter.filter(|id| filters::find(id).is_some());
        self.filter_strength = if self.filter_strength.is_finite() { self.filter_strength.clamp(0.0, 1.0) } else { 1.0 };
        self.blur = finite(self.blur, 0.0, 100.0);
        self.blur_style = if self.blur > 0.0 { self.blur_style.resolved() } else { BlurStyle::Lens };
        self
    }

    /// True when the recipe changes nothing.
    pub fn is_identity(&self) -> bool {
        self.rotate == 0
            && !self.flip
            && self.straighten == 0.0
            && self.crop.is_none()
            && self.adjust.is_zero()
            && (self.filter.is_none() || self.filter_strength == 0.0)
            && self.blur == 0.0
    }

    pub fn has_geometry(&self) -> bool {
        self.rotate != 0 || self.flip || self.straighten != 0.0 || self.crop.is_some()
    }

    /// The same edit without crop, rotation or flip (what the crop screen shows under its frame).
    pub fn without_geometry(&self) -> Edit {
        Edit { rotate: 0, flip: false, straighten: 0.0, crop: None, ..self.clone() }
    }

    /// Size after rotation and crop, for a picture of `w` x `h` (as displayed, EXIF rotation applied).
    pub fn output_size(&self, w: u32, h: u32) -> (u32, u32) {
        let (bw, bh) = if self.rotate % 2 == 1 { (h, w) } else { (w, h) };
        match self.crop {
            Some(c) => (((c.w * bw as f32).round() as u32).max(1), ((c.h * bh as f32).round() as u32).max(1)),
            None => (bw, bh),
        }
    }

    /// How big the source must be decoded (as displayed) for a render to fill `max_w` x
    /// `max_h` after cropping: larger than the box when the crop is small.
    pub fn source_box(&self, max_w: u32, max_h: u32) -> (u32, u32) {
        let zoom = self.crop.map(|c| 1.0 / c.w.min(c.h)).unwrap_or(1.0).clamp(1.0, 8.0);
        let (w, h) = ((max_w as f32 * zoom) as u32, (max_h as f32 * zoom) as u32);
        if self.rotate % 2 == 1 { (h, w) } else { (w, h) }
    }
}

// ---------- storage ----------

/// An item's saved edit.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub edit: Option<Edit>,
    /// When it was last edited or reverted (unix ms); part of picture URLs.
    pub edited_at: Option<i64>,
}

pub fn get(db: &Db, id: i64) -> Result<Saved> {
    let conn = db.conn()?;
    let row: Option<(Option<String>, Option<i64>)> = conn
        .query_row("SELECT edit, edited_at FROM media WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let (edit, edited_at) = row.ok_or(Error::NotFound)?;
    Ok(Saved { edit: edit.and_then(|s| parse(&s)), edited_at })
}

pub(crate) fn parse(s: &str) -> Option<Edit> {
    serde_json::from_str::<Edit>(s).ok().map(Edit::normalized).filter(|e| !e.is_identity())
}

/// `edit`, `edited_at`, `edit_width`, `edit_height`.
pub(crate) type Columns = (Option<String>, i64, Option<u32>, Option<u32>);

/// Columns for an edit: the recipe, a new edit time and the size it gives.
/// The time is at least a second after the item's previous version, so picture URLs
/// (which carry seconds) change.
pub(crate) fn columns(
    conn: &r2d2_sqlite::rusqlite::Connection,
    id: i64,
    edit: Option<&Edit>,
) -> Result<Columns> {
    let (w, h, mtime, edited): (Option<u32>, Option<u32>, i64, Option<i64>) = conn
        .query_row("SELECT width, height, mtime, edited_at FROM media WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()?
        .ok_or(Error::NotFound)?;
    let previous = mtime.max(edited.unwrap_or(0));
    let at = chrono::Utc::now().timestamp_millis().max(previous + 1000);
    let edit = edit.filter(|e| !e.is_identity());
    let size = match (edit, w, h) {
        (Some(e), Some(w), Some(h)) if e.has_geometry() => Some(e.output_size(w, h)),
        _ => None,
    };
    Ok((edit.map(serde_json::to_string).transpose()?, at, size.map(|s| s.0), size.map(|s| s.1)))
}

/// Save an item's edit (`None` or a recipe that changes nothing reverts to the original).
/// Recorded for `.memora` like favorites. Returns the stored state.
pub fn save(db: &Db, id: i64, edit: Option<Edit>) -> Result<Saved> {
    let edit = edit.map(Edit::normalized).filter(|e| !e.is_identity());
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let kind: String = tx.query_row("SELECT kind FROM media WHERE id = ?1", [id], |r| r.get(0)).optional()?.ok_or(Error::NotFound)?;
    if kind != "photo" {
        return Err(Error::media("only photos can be edited"));
    }
    let (json, at, w, h) = columns(&tx, id, edit.as_ref())?;
    tx.execute(
        "UPDATE media SET edit = ?2, edited_at = ?3, edit_width = ?4, edit_height = ?5 WHERE id = ?1",
        params![id, json, at, w, h],
    )?;
    let value = match &edit {
        Some(e) => serde_json::to_value(e)?,
        None => serde_json::Value::Null,
    };
    crate::sync::record_media(&tx, &[id], "edit", value)?;
    tx.commit()?;
    Ok(Saved { edit, edited_at: Some(at) })
}

/// Media ids that have an edit, among `ids`.
pub fn edited_among(db: &Db, ids: &[i64]) -> Result<Vec<i64>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare_cached("SELECT id FROM media WHERE edit IS NOT NULL AND id IN (SELECT value FROM json_each(?1))")?;
    Ok(stmt.query_map([serde_json::to_string(ids)?], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?)
}

#[cfg(test)]
mod tests;
