//! Storage side of on-device recognition. The models run in `photos-ml`, which takes
//! items at [`Stage::HASH`] and stores what it found here, moving them to [`Stage::AI`].

use r2d2_sqlite::rusqlite::params;

use crate::db::Db;
use crate::media::{MEDIA_COLUMNS, Media, Stage};
use crate::people::{AUTO, FACE_DIM, to_blob};
use crate::semantic::{self, CLIP_DIM};
use crate::Result;

/// Which models made the stored faces and CLIP vectors (`photos-ml` checks it matches its
/// model files). Index snapshots from PCs with other models don't bring their faces.
pub const RECOGNITION: &str = "yunet-2023mar/sface-2021dec/clip-vit-base-patch32-quantized";

/// A face found in a picture.
#[derive(Debug, Clone)]
pub struct FoundFace {
    /// x, y, width, height as fractions of the displayed picture.
    pub rect: [f32; 4],
    pub score: f32,
    /// Unit-length SFace vector.
    pub embedding: Vec<f32>,
}

/// What recognition found in one item. Empty when the item could not be read.
#[derive(Debug, Clone, Default)]
pub struct Found {
    pub faces: Vec<FoundFace>,
    pub clip: Option<Vec<f32>>,
}

/// What [`save`] changed.
#[derive(Debug, Default)]
pub struct Saved {
    /// Ids of the stored faces, in the order given.
    pub face_ids: Vec<i64>,
    /// Faces the item had before, now gone (their pictures can be deleted).
    pub removed_faces: Vec<i64>,
}

/// Items waiting for recognition, newest first.
pub fn next_batch(db: &Db, limit: usize) -> Result<Vec<Media>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {MEDIA_COLUMNS} FROM media WHERE missing = 0 AND stage = ?1
         ORDER BY taken_at DESC, id DESC LIMIT ?2"
    ))?;
    let items = stmt.query_map(params![Stage::HASH, limit as i64], Media::from_row)?;
    Ok(items.collect::<std::result::Result<_, _>>()?)
}

pub(crate) fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let w = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
    let h = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let inter = w * h;
    inter / (a[2] * a[3] + b[2] * b[3] - inter)
}

/// Store an item's faces and CLIP vector and mark it done. Returns None (and stores nothing)
/// if the item changed on disk in the meantime; it will be looked at again.
///
/// An item looked at again (its file changed) keeps the people of faces found in the same
/// place, so names and the user's corrections survive.
pub fn save(db: &Db, m: &Media, found: &Found) -> Result<Option<Saved>> {
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let n = tx
        .prepare_cached("UPDATE media SET stage = ?3 WHERE id = ?1 AND stage = ?2 AND mtime = ?4")?
        .execute(params![m.id, Stage::HASH, Stage::AI, m.mtime])?;
    if n == 0 {
        return Ok(None);
    }

    let old: Vec<(i64, [f32; 4], Option<i64>, i64)> = {
        let mut stmt = tx.prepare_cached("SELECT id, x, y, w, h, person_id, locked FROM faces WHERE media_id = ?1")?;
        stmt.query_map([m.id], |r| Ok((r.get(0)?, [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?], r.get(5)?, r.get(6)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    tx.prepare_cached("DELETE FROM faces WHERE media_id = ?1")?.execute([m.id])?;

    let mut saved = Saved { removed_faces: old.iter().map(|o| o.0).collect(), ..Default::default() };
    for face in found.faces.iter().filter(|f| f.embedding.len() == FACE_DIM) {
        let (person, locked) = old
            .iter()
            .filter(|o| o.2.is_some() || o.3 != AUTO)
            .map(|o| (iou(o.1, face.rect), o))
            .filter(|(overlap, _)| *overlap > 0.5)
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, o)| (o.2, o.3))
            .unwrap_or((None, AUTO));
        let [x, y, w, h] = face.rect;
        tx.prepare_cached(
            "INSERT INTO faces (media_id, x, y, w, h, score, embedding, person_id, locked)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?
        .execute(params![m.id, x, y, w, h, face.score, to_blob(&face.embedding), person, locked])?;
        saved.face_ids.push(tx.last_insert_rowid());
    }
    // Names and corrections made on other PCs for this item (see `sync`).
    crate::sync::apply_faces_for(&tx, m.id)?;
    // ...and those of a restored backup.
    crate::backup::apply_waiting_faces(&tx, m.id)?;
    match &found.clip {
        Some(v) if v.len() == CLIP_DIM => semantic::save(&tx, m.id, v)?,
        _ => {
            tx.prepare_cached("DELETE FROM clip_embeddings WHERE media_id = ?1")?.execute([m.id])?;
        }
    }
    tx.commit()?;
    Ok(Some(saved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::people::{self, tests::face_like};

    #[test]
    fn saves_and_keeps_people_when_looked_at_again() {
        let (_dir, db) = test_db();
        let ids = people::tests::insert_media(&db, 1);
        db.conn().unwrap().execute("UPDATE media SET stage = 3", []).unwrap();
        let m = crate::media::get(&db, ids[0]).unwrap();
        assert_eq!(next_batch(&db, 10).unwrap().len(), 1);

        let face = |x: f32, seed| FoundFace { rect: [x, 0.2, 0.1, 0.1], score: 0.9, embedding: face_like(0, seed) };
        let found = Found { faces: vec![face(0.1, 1), face(0.6, 2)], clip: Some(vec![0.0; CLIP_DIM]) };
        let saved = save(&db, &m, &found).unwrap().unwrap();
        assert_eq!(saved.face_ids.len(), 2);
        assert!(next_batch(&db, 10).unwrap().is_empty());
        // Done items are not saved twice.
        assert!(save(&db, &m, &found).unwrap().is_none());

        // The user names the first face; then the file changes and is looked at again.
        let p = people::new_person_for_face(&db, saved.face_ids[0], Some("Ria")).unwrap();
        db.conn().unwrap().execute("UPDATE media SET stage = 3", []).unwrap();
        let again = Found { faces: vec![face(0.11, 3)], clip: None };
        let saved2 = save(&db, &m, &again).unwrap().unwrap();
        assert_eq!(saved2.removed_faces, saved.face_ids);
        let faces = people::faces_of(&db, m.id).unwrap();
        assert_eq!(faces.len(), 1);
        assert_eq!(faces[0].person.as_ref().map(|p| p.id), Some(p.id));
        let clips: i64 = db.conn().unwrap().query_row("SELECT count(*) FROM clip_embeddings", [], |r| r.get(0)).unwrap();
        assert_eq!(clips, 0);
    }
}
