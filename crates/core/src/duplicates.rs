//! Duplicate detection.
//!
//! - Exact: same `blake3`, so the same bytes.
//! - Near: perceptual hashes within a few bits (a BK-tree makes the search fast), and a
//!   similar shape. Videos are compared by their poster frame and need a similar duration.
//!
//! Groups are computed on request from the hashes in the database; nothing is stored.

use std::collections::HashMap;

use r2d2_sqlite::rusqlite::Row;
use serde::Serialize;

use crate::db::Db;
use crate::hash::hamming;
use crate::media::Stage;
use crate::Result;

/// Pictures whose hashes differ in at most this many of 64 bits count as near-duplicates.
pub const NEAR_DISTANCE: u32 = 6;
/// Largest relative difference in aspect ratio between near-duplicates.
const ASPECT_TOLERANCE: f64 = 0.05;
/// Largest difference in duration between near-duplicate videos.
const DURATION_TOLERANCE_MS: i64 = 1000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupItem {
    pub id: i64,
    pub name: String,
    pub path: String,
    /// Size on screen; 0 when unknown.
    pub w: u32,
    pub h: u32,
    pub size: i64,
    /// Taken at, local time.
    pub taken_at: String,
    /// File modification time in seconds, for the thumbnail URL.
    pub m: i64,
    pub video: bool,
    pub duration_ms: Option<i64>,
    /// The copy to keep, by [`keeper_rank`].
    pub keeper: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Group {
    /// "exact" (identical files) or "near" (look the same).
    pub kind: &'static str,
    /// The suggested keeper comes first.
    pub items: Vec<DupItem>,
    /// Bytes freed by removing everything but the keeper.
    pub reclaimable: i64,
}

struct Candidate {
    item: DupItem,
    blake3: String,
    phash: Option<u64>,
    has_exif: bool,
}

fn candidate(r: &Row) -> r2d2_sqlite::rusqlite::Result<Candidate> {
    Ok(Candidate {
        item: DupItem {
            id: r.get(0)?,
            name: r.get(1)?,
            path: r.get(2)?,
            w: r.get::<_, Option<u32>>(3)?.unwrap_or(0),
            h: r.get::<_, Option<u32>>(4)?.unwrap_or(0),
            size: r.get(5)?,
            taken_at: r.get(6)?,
            m: r.get::<_, i64>(7)? / 1000,
            video: r.get::<_, String>(8)? == "video",
            duration_ms: r.get(9)?,
            keeper: false,
        },
        blake3: r.get(10)?,
        phash: r.get::<_, Option<i64>>(11)?.map(|h| h as u64),
        has_exif: r.get(12)?,
    })
}

/// Which copy to keep: highest resolution, then one with camera/GPS data, then the oldest.
/// Smaller sorts first.
fn keeper_rank(c: &Candidate) -> (std::cmp::Reverse<u64>, bool, &str, i64) {
    let pixels = c.item.w as u64 * c.item.h as u64;
    (std::cmp::Reverse(pixels), !c.has_exif, &c.item.taken_at, c.item.id)
}

/// All groups of duplicate items, the ones that free most space first.
pub fn find(db: &Db) -> Result<Vec<Group>> {
    let candidates: Vec<Candidate> = {
        let conn = db.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, path, width, height, size, taken_at, mtime, kind, duration_ms, blake3, phash,
                    (camera_make IS NOT NULL OR lat IS NOT NULL)
             FROM media
             WHERE missing = 0 AND trashed_at IS NULL AND stage >= ?1 AND error IS NULL AND blake3 IS NOT NULL",
        )?;
        stmt.query_map([Stage::HASH], candidate)?.collect::<std::result::Result<_, _>>()?
    };

    let mut sets = DisjointSets::new(candidates.len());

    // Exact: same bytes.
    let mut by_bytes: HashMap<&str, usize> = HashMap::new();
    for (i, c) in candidates.iter().enumerate() {
        match by_bytes.get(c.blake3.as_str()) {
            Some(&first) => sets.union(first, i),
            None => {
                by_bytes.insert(&c.blake3, i);
            }
        }
    }

    // Near: similar picture and shape.
    let mut tree = BkTree::default();
    for (i, c) in candidates.iter().enumerate() {
        let Some(hash) = c.phash else { continue };
        for j in tree.search(hash, NEAR_DISTANCE) {
            if similar_shape(&candidates[i], &candidates[j]) {
                sets.union(i, j);
            }
        }
        tree.insert(hash, i);
    }

    let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..candidates.len() {
        members.entry(sets.find(i)).or_default().push(i);
    }

    let mut candidates: Vec<Option<Candidate>> = candidates.into_iter().map(Some).collect();
    let mut groups = Vec::new();
    for mut indices in members.into_values().filter(|m| m.len() > 1) {
        let mut group: Vec<Candidate> = indices.drain(..).map(|i| candidates[i].take().unwrap()).collect();
        group.sort_by(|a, b| keeper_rank(a).cmp(&keeper_rank(b)));
        let exact = group.iter().all(|c| c.blake3 == group[0].blake3);
        let reclaimable = group.iter().skip(1).map(|c| c.item.size).sum();
        let mut items: Vec<DupItem> = group.into_iter().map(|c| c.item).collect();
        items[0].keeper = true;
        groups.push(Group { kind: if exact { "exact" } else { "near" }, items, reclaimable });
    }
    groups.sort_by(|a, b| {
        b.reclaimable.cmp(&a.reclaimable).then_with(|| a.items[0].id.cmp(&b.items[0].id))
    });
    Ok(groups)
}

fn similar_shape(a: &Candidate, b: &Candidate) -> bool {
    if a.item.video != b.item.video {
        return false;
    }
    if a.item.w > 0 && a.item.h > 0 && b.item.w > 0 && b.item.h > 0 {
        let (ra, rb) = (a.item.w as f64 / a.item.h as f64, b.item.w as f64 / b.item.h as f64);
        if (ra - rb).abs() / ra.max(rb) > ASPECT_TOLERANCE {
            return false;
        }
    }
    if a.item.video {
        return match (a.item.duration_ms, b.item.duration_ms) {
            (Some(x), Some(y)) => (x - y).abs() <= DURATION_TOLERANCE_MS,
            _ => false,
        };
    }
    true
}

// ---------- data structures ----------

struct DisjointSets {
    parent: Vec<usize>,
}

impl DisjointSets {
    fn new(n: usize) -> Self {
        DisjointSets { parent: (0..n).collect() }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb.max(ra)] = ra.min(rb);
        }
    }
}

/// Burkhard-Keller tree over 64-bit hashes with Hamming distance: finds everything
/// within a small distance without comparing against every hash.
#[derive(Default)]
struct BkTree {
    nodes: Vec<BkNode>,
}

struct BkNode {
    hash: u64,
    id: usize,
    /// (distance to this node, child node index)
    children: Vec<(u32, usize)>,
}

impl BkTree {
    fn insert(&mut self, hash: u64, id: usize) {
        if self.nodes.is_empty() {
            self.nodes.push(BkNode { hash, id, children: Vec::new() });
            return;
        }
        let mut at = 0;
        loop {
            let d = hamming(hash, self.nodes[at].hash);
            match self.nodes[at].children.iter().find(|(cd, _)| *cd == d) {
                Some(&(_, child)) => at = child,
                None => {
                    let new = self.nodes.len();
                    self.nodes.push(BkNode { hash, id, children: Vec::new() });
                    self.nodes[at].children.push((d, new));
                    return;
                }
            }
        }
    }

    /// Ids of all entries within `max` bits of `hash`.
    fn search(&self, hash: u64, max: u32) -> Vec<usize> {
        let mut found = Vec::new();
        let mut stack = if self.nodes.is_empty() { vec![] } else { vec![0] };
        while let Some(at) = stack.pop() {
            let node = &self.nodes[at];
            let d = hamming(hash, node.hash);
            if d <= max {
                found.push(node.id);
            }
            // Triangle inequality: only children at distance d-max..=d+max can match.
            for &(cd, child) in &node.children {
                if cd + max >= d && cd <= d + max {
                    stack.push(child);
                }
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use r2d2_sqlite::rusqlite::params;

    #[test]
    fn bk_tree_matches_brute_force() {
        // Pseudo-random hashes, with clusters of near-copies.
        let mut x = 0x9E3779B97F4A7C15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let mut hashes = Vec::new();
        for _ in 0..300 {
            let base = next();
            hashes.push(base);
            hashes.push(base ^ (1 << (next() % 64)));
            hashes.push(base ^ (1 << (next() % 64)) ^ (1 << (next() % 64)));
        }
        let mut tree = BkTree::default();
        for (i, h) in hashes.iter().enumerate() {
            tree.insert(*h, i);
        }
        for probe in hashes.iter().step_by(7) {
            let mut got = tree.search(*probe, NEAR_DISTANCE);
            got.sort();
            let want: Vec<usize> =
                (0..hashes.len()).filter(|&i| hamming(*probe, hashes[i]) <= NEAR_DISTANCE).collect();
            assert_eq!(got, want);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        db: &Db,
        name: &str,
        blake: &str,
        phash: Option<u64>,
        (w, h): (u32, u32),
        size: i64,
        taken: &str,
        exif: bool,
    ) -> i64 {
        let conn = db.conn().unwrap();
        conn.execute("INSERT OR IGNORE INTO folders (id, path, added_at) VALUES (1, 'C:/x', '')", []).unwrap();
        conn.execute(
            "INSERT INTO media (folder_id, path, name, size, mtime, kind, mime, width, height, taken_at, camera_make,
                                blake3, phash, stage, added_at)
             VALUES (1, ?1, ?1, ?2, 1000, 'photo', 'image/jpeg', ?3, ?4, ?5, ?6, ?7, ?8, 3, '')",
            params![name, size, w, h, taken, exif.then_some("Cam"), blake, phash.map(|p| p as i64)],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn groups_exact_and_near_with_a_keeper() {
        let (_dir, db) = test_db();
        // An exact pair (same bytes).
        let a1 = add(&db, "a1.jpg", "AAA", Some(0xF0F0), (4000, 3000), 5000, "2024-01-02T10:00:00", true);
        let a2 = add(&db, "a2.jpg", "AAA", Some(0xF0F0), (4000, 3000), 5000, "2024-01-01T10:00:00", true);
        // A resized copy (3 bits away) of the same scene, plus an unrelated picture.
        let b1 = add(&db, "b1.jpg", "BBB", Some(0xFF00FF00), (4000, 3000), 4000, "2024-02-01T10:00:00", false);
        let b2 = add(&db, "b2.jpg", "CCC", Some(0xFF00FF00 ^ 0b111), (1000, 750), 300, "2024-02-01T10:00:00", true);
        let other = add(&db, "other.jpg", "DDD", Some(0xFFFF_FFFF_0000_0000), (4000, 3000), 100, "2024-03-01T10:00:00", true);

        let groups = find(&db).unwrap();
        assert_eq!(groups.len(), 2);
        let ids = |g: &Group| g.items.iter().map(|i| i.id).collect::<Vec<_>>();

        // Largest saving first: the exact pair frees 5000 bytes, the near pair 300.
        assert_eq!((groups[0].kind, groups[0].reclaimable), ("exact", 5000));
        assert_eq!(ids(&groups[0]), vec![a2, a1], "same size: the oldest is kept");
        assert!(groups[0].items[0].keeper && !groups[0].items[1].keeper);

        assert_eq!((groups[1].kind, groups[1].reclaimable), ("near", 300));
        assert_eq!(ids(&groups[1]), vec![b1, b2], "the higher resolution is kept");
        assert!(!groups.iter().any(|g| ids(g).contains(&other)));
    }

    #[test]
    fn keeper_prefers_exif_over_age_at_equal_resolution() {
        let (_dir, db) = test_db();
        let old_bare = add(&db, "old.jpg", "X1", Some(0xAAAA_5555), (800, 600), 10, "2020-01-01T00:00:00", false);
        let newer_exif = add(&db, "new.jpg", "X2", Some(0xAAAA_5555), (800, 600), 10, "2021-01-01T00:00:00", true);
        let g = &find(&db).unwrap()[0];
        assert_eq!((g.items[0].id, g.items[1].id), (newer_exif, old_bare));
    }

    #[test]
    fn near_matches_need_a_similar_shape_and_valid_items() {
        let (_dir, db) = test_db();
        add(&db, "wide.jpg", "P1", Some(0x1234_5678), (4000, 3000), 10, "2024-01-01T00:00:00", true);
        add(&db, "tall.jpg", "P2", Some(0x1234_5678), (3000, 4000), 10, "2024-01-01T00:00:00", true);
        add(&db, "nohash.jpg", "P3", None, (4000, 3000), 10, "2024-01-01T00:00:00", true);
        assert!(find(&db).unwrap().is_empty(), "different aspect ratios, and no hash, are not near-duplicates");

        // Trashed and failed items never show up.
        let t = add(&db, "trashed.jpg", "P4", Some(0x1234_5678), (4000, 3000), 10, "2024-01-01T00:00:00", true);
        assert_eq!(find(&db).unwrap().len(), 1);
        db.conn().unwrap().execute("UPDATE media SET trashed_at = 'x' WHERE id = ?1", [t]).unwrap();
        assert!(find(&db).unwrap().is_empty());
        db.conn().unwrap().execute("UPDATE media SET trashed_at = NULL, error = 'bad' WHERE id = ?1", [t]).unwrap();
        assert!(find(&db).unwrap().is_empty());
    }

    #[test]
    fn videos_need_similar_durations() {
        let (_dir, db) = test_db();
        let v1 = add(&db, "v1.mp4", "V1", Some(0xBEEF_0000), (1920, 1080), 10, "2024-01-01T00:00:00", true);
        let v2 = add(&db, "v2.mp4", "V2", Some(0xBEEF_0000), (1920, 1080), 10, "2024-01-01T00:00:01", true);
        let conn = db.conn().unwrap();
        for (id, ms) in [(v1, 10_000), (v2, 10_400)] {
            conn.execute("UPDATE media SET kind = 'video', duration_ms = ?2 WHERE id = ?1", params![id, ms]).unwrap();
        }
        assert_eq!(find(&db).unwrap().len(), 1);
        conn.execute("UPDATE media SET duration_ms = 30000 WHERE id = ?1", [v2]).unwrap();
        assert!(find(&db).unwrap().is_empty());
    }
}
