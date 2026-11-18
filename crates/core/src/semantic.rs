//! Search by what is in the picture: CLIP image vectors, compared with a text's vector.
//!
//! Raw CLIP scores sit in a narrow band and each picture has its own baseline (some score
//! high for any text), so a nonsense word can outscore a real one. Callers therefore pass
//! the query's vector minus the vector of a neutral text ("a photo of"): what is left
//! measures how much better the picture fits this text than any text.
//!
//! All vectors are kept in memory (2 KB per item) and compared one by one, which takes a
//! few milliseconds even for 100k items. The text encoder lives in `photos-ml`.

use std::collections::HashMap;
use std::sync::RwLock;

use r2d2_sqlite::rusqlite::Transaction;
use rayon::prelude::*;

use crate::db::Db;
use crate::people::{dot, from_blob, to_blob};
use crate::Result;

/// Length of a CLIP ViT-B/32 vector.
pub const CLIP_DIM: usize = 512;

/// A picture matches when it fits the text this much better than the neutral text...
pub const MIN_SCORE: f32 = 0.02;
/// ...and at least this share of the best match, so a query with a few strong hits doesn't
/// also list everything vaguely similar.
pub const SHARE_OF_BEST: f32 = 0.5;

#[derive(Default)]
pub struct Index {
    state: RwLock<Option<State>>,
}

#[derive(Default)]
struct State {
    ids: Vec<i64>,
    vecs: Vec<f32>,
    pos: HashMap<i64, usize>,
}

impl State {
    fn put(&mut self, id: i64, v: &[f32]) {
        match self.pos.get(&id) {
            Some(&i) => self.vecs[i * CLIP_DIM..(i + 1) * CLIP_DIM].copy_from_slice(v),
            None => {
                self.pos.insert(id, self.ids.len());
                self.ids.push(id);
                self.vecs.extend_from_slice(v);
            }
        }
    }
}

impl Index {
    fn load(db: &Db) -> Result<State> {
        let conn = db.conn()?;
        let mut stmt = conn.prepare("SELECT media_id, vec FROM clip_embeddings")?;
        let mut state = State::default();
        for row in stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)))? {
            let (id, blob) = row?;
            let v = from_blob(&blob);
            if v.len() == CLIP_DIM {
                state.put(id, &v);
            }
        }
        Ok(state)
    }

    /// Add or replace an item's vector (after the database has it).
    pub fn put(&self, id: i64, v: &[f32]) {
        if v.len() != CLIP_DIM {
            return;
        }
        // Not loaded yet: the first search loads it from the database anyway.
        if let Some(state) = self.state.write().unwrap().as_mut() {
            state.put(id, v);
        }
    }

    /// Forget the vectors held in memory; the next search loads them again (after many
    /// were added at once, e.g. from an index snapshot).
    pub fn reset(&self) {
        *self.state.write().unwrap() = None;
    }

    /// Items whose picture matches the query (a text vector minus a neutral text's vector),
    /// best first, with their scores.
    /// Items that left the library may still be listed; callers filter by their own query.
    pub fn search(&self, db: &Db, text: &[f32]) -> Result<Vec<(i64, f32)>> {
        if self.state.read().unwrap().is_none() {
            let loaded = Self::load(db)?;
            let mut state = self.state.write().unwrap();
            if state.is_none() {
                *state = Some(loaded);
            }
        }
        let guard = self.state.read().unwrap();
        let state = guard.as_ref().expect("loaded above");
        let scores: Vec<f32> = state.vecs.par_chunks_exact(CLIP_DIM).map(|v| dot(v, text)).collect();
        Ok(select(&state.ids, &scores))
    }
}

/// Matches by the thresholds above, best first.
fn select(ids: &[i64], scores: &[f32]) -> Vec<(i64, f32)> {
    let best = scores.iter().copied().fold(f32::MIN, f32::max);
    let cut = MIN_SCORE.max(best * SHARE_OF_BEST);
    let mut hits: Vec<(i64, f32)> =
        ids.iter().zip(scores).filter(|(_, s)| **s >= cut).map(|(id, s)| (*id, *s)).collect();
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    hits
}

pub(crate) fn save(tx: &Transaction, media_id: i64, v: &[f32]) -> Result<()> {
    tx.prepare_cached("INSERT OR REPLACE INTO clip_embeddings (media_id, vec) VALUES (?1, ?2)")?
        .execute(r2d2_sqlite::rusqlite::params![media_id, to_blob(v)])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_strong_matches() {
        let ids = [1, 2, 3, 4];
        // Clear winners: the weak one is dropped even though it passes MIN_SCORE.
        assert_eq!(select(&ids, &[0.06, 0.05, 0.025, -0.01]).iter().map(|h| h.0).collect::<Vec<_>>(), [1, 2]);
        // Nothing good enough.
        assert!(select(&ids, &[0.015, 0.012, 0.0, -0.02]).is_empty());
        assert!(select(&[], &[]).is_empty());
    }
}
