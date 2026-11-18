//! People: faces found in photos, grouped into persons.
//!
//! Grouping is automatic (see [`cluster`]) but the user always wins: faces they moved
//! or removed are never touched again, merges are kept, and names are theirs.

use std::collections::{BinaryHeap, HashMap, HashSet};

use chrono::Utc;
use r2d2_sqlite::rusqlite::{OptionalExtension, Row, params};
use rayon::prelude::*;
use serde::Serialize;

use crate::db::Db;
use crate::{Error, Result, sync};

/// Length of a face embedding (SFace).
pub const FACE_DIM: usize = 128;

/// `faces.locked` values.
pub const AUTO: i64 = 0;
pub const BY_USER: i64 = 1;
pub const NOT_THIS_PERSON: i64 = 2;

const MAX_NAME: usize = 100;
use crate::media::VISIBLE_M as VISIBLE;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub id: i64,
    pub name: Option<String>,
    pub hidden: bool,
    /// Visible photos and videos they appear in.
    pub count: i64,
    /// Face shown for them: the chosen one, else their clearest.
    pub cover_face: Option<i64>,
}

/// A face in one photo, for the viewer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Face {
    pub id: i64,
    /// Box as fractions of the displayed picture.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub person: Option<PersonRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonRef {
    pub id: i64,
    pub name: Option<String>,
    pub hidden: bool,
}

fn select_persons(filter: &str) -> String {
    format!(
        "SELECT p.id, p.name, p.hidden,
            (SELECT count(DISTINCT f.media_id) FROM faces f JOIN media m ON m.id = f.media_id
              WHERE f.person_id = p.id AND {VISIBLE}) AS cnt,
            coalesce(
              (SELECT f.id FROM faces f JOIN media m ON m.id = f.media_id
                WHERE f.id = p.cover_face_id AND f.person_id = p.id AND {VISIBLE}),
              (SELECT f.id FROM faces f JOIN media m ON m.id = f.media_id
                WHERE f.person_id = p.id AND {VISIBLE}
                ORDER BY f.score * min(f.w * f.h, 0.05) DESC, f.id LIMIT 1))
         FROM persons p {filter}"
    )
}

fn person_row(r: &Row) -> r2d2_sqlite::rusqlite::Result<Person> {
    Ok(Person { id: r.get(0)?, name: r.get(1)?, hidden: r.get(2)?, count: r.get(3)?, cover_face: r.get(4)? })
}

/// People with at least one visible item, most photographed first. Hidden people only if asked.
pub fn list(db: &Db, include_hidden: bool) -> Result<Vec<Person>> {
    let conn = db.conn()?;
    let filter = if include_hidden { "" } else { "WHERE p.hidden = 0" };
    let mut stmt = conn.prepare(&select_persons(filter))?;
    let mut people: Vec<Person> =
        stmt.query_map([], person_row)?.filter(|p| !matches!(p, Ok(p) if p.count == 0)).collect::<std::result::Result<_, _>>()?;
    people.sort_by(|a, b| b.count.cmp(&a.count).then(a.id.cmp(&b.id)));
    Ok(people)
}

pub fn get(db: &Db, id: i64) -> Result<Person> {
    db.conn()?
        .query_row(&select_persons("WHERE p.id = ?1"), [id], person_row)
        .optional()?
        .ok_or(Error::NotFound)
}

/// Name a person (None or blank removes the name).
pub fn rename(db: &Db, id: i64, name: Option<&str>) -> Result<Person> {
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    if name.is_some_and(|n| n.chars().count() > MAX_NAME) {
        return Err(Error::invalid(format!("names can have at most {MAX_NAME} characters")));
    }
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let n = tx.execute("UPDATE persons SET name = ?2 WHERE id = ?1", params![id, name])?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    sync::record_person(&tx, id)?;
    tx.commit()?;
    drop(conn);
    get(db, id)
}

pub fn set_hidden(db: &Db, id: i64, hidden: bool) -> Result<Person> {
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let n = tx.execute("UPDATE persons SET hidden = ?2 WHERE id = ?1", params![id, hidden])?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    sync::record_person(&tx, id)?;
    tx.commit()?;
    drop(conn);
    get(db, id)
}

/// Show this face for the person.
pub fn set_cover(db: &Db, id: i64, face_id: i64) -> Result<Person> {
    let n = db.conn()?.execute(
        "UPDATE persons SET cover_face_id = ?2
         WHERE id = ?1 AND EXISTS (SELECT 1 FROM faces WHERE id = ?2 AND person_id = ?1)",
        params![id, face_id],
    )?;
    if n == 0 {
        get(db, id)?;
        return Err(Error::invalid("that face is not this person's"));
    }
    get(db, id)
}

/// Make `from` people part of `into`. Their faces count as confirmed by the user, so automatic
/// grouping never splits them again. `into` keeps its name, or takes the first name found.
pub fn merge(db: &Db, from: &[i64], into: i64) -> Result<Person> {
    let target = get(db, into)?;
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let mut name = target.name;
    for &id in from.iter().filter(|&&id| id != into) {
        let other: Option<Option<String>> =
            tx.query_row("SELECT name FROM persons WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        let Some(other_name) = other else { return Err(Error::NotFound) };
        name = name.or(other_name);
        tx.execute("UPDATE faces SET person_id = ?2, locked = ?3 WHERE person_id = ?1", params![id, into, BY_USER])?;
        tx.execute("DELETE FROM persons WHERE id = ?1", [id])?;
    }
    tx.execute("UPDATE persons SET name = ?2 WHERE id = ?1", params![into, name])?;
    sync::record_person(&tx, into)?;
    tx.commit()?;
    drop(conn);
    get(db, into)
}

/// Faces in one photo or video, left to right.
pub fn faces_of(db: &Db, media_id: i64) -> Result<Vec<Face>> {
    let conn = db.conn()?;
    let mut stmt = conn.prepare_cached(
        "SELECT f.id, f.x, f.y, f.w, f.h, p.id, p.name, p.hidden
         FROM faces f LEFT JOIN persons p ON p.id = f.person_id
         WHERE f.media_id = ?1 ORDER BY f.x",
    )?;
    let faces = stmt
        .query_map([media_id], |r| {
            let person = match r.get::<_, Option<i64>>(5)? {
                Some(id) => Some(PersonRef { id, name: r.get(6)?, hidden: r.get(7)? }),
                None => None,
            };
            Ok(Face { id: r.get(0)?, x: r.get(1)?, y: r.get(2)?, w: r.get(3)?, h: r.get(4)?, person })
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(faces)
}

/// The photo or video a face is in.
pub fn media_of_face(db: &Db, face_id: i64) -> Result<(i64, [f32; 4])> {
    db.conn()?
        .query_row("SELECT media_id, x, y, w, h FROM faces WHERE id = ?1", [face_id], |r| {
            Ok((r.get(0)?, [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?]))
        })
        .optional()?
        .ok_or(Error::NotFound)
}

/// Put a face with a person (chosen by the user), or with nobody: "not this person",
/// which also keeps automatic grouping from putting it anywhere again.
pub fn set_face_person(db: &Db, face_id: i64, person: Option<i64>) -> Result<()> {
    if let Some(p) = person {
        get(db, p)?;
    }
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    let locked = if person.is_some() { BY_USER } else { NOT_THIS_PERSON };
    let n = tx.execute("UPDATE faces SET person_id = ?2, locked = ?3 WHERE id = ?1", params![face_id, person, locked])?;
    if n == 0 {
        return Err(Error::NotFound);
    }
    sync::record_faces(&tx, &[face_id])?;
    tx.commit()?;
    Ok(())
}

/// A person for a face that has none yet (or a new one-face person), chosen by the user.
pub fn new_person_for_face(db: &Db, face_id: i64, name: Option<&str>) -> Result<Person> {
    media_of_face(db, face_id)?;
    let id = {
        let conn = db.conn()?;
        conn.execute("INSERT INTO persons (created_at) VALUES (?1)", [Utc::now().to_rfc3339()])?;
        conn.last_insert_rowid()
    };
    set_face_person(db, face_id, Some(id))?;
    rename(db, id, name)
}

/// Items showing a named, visible person whose name matches the search text: every word of
/// the text starts a word of the name ("ann" finds "Anna Roy").
pub fn media_named(db: &Db, text: &str) -> Result<Vec<i64>> {
    let words = name_words(text);
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let conn = db.conn()?;
    let mut stmt = conn.prepare_cached("SELECT id, name FROM persons WHERE name IS NOT NULL AND hidden = 0")?;
    let matching: Vec<i64> = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .filter_map(|r| r.ok())
        .filter(|(_, name)| {
            let name_words = name_words(name);
            words.iter().all(|w| name_words.iter().any(|n| n.starts_with(w.as_str())))
        })
        .map(|(id, _)| id)
        .collect();
    if matching.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare_cached(
        "SELECT DISTINCT media_id FROM faces WHERE person_id IN (SELECT value FROM json_each(?1))",
    )?;
    let ids = stmt
        .query_map([serde_json::to_string(&matching)?], |r| r.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    Ok(ids)
}

fn name_words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

// ---------- embeddings ----------

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    // Eight running sums let the compiler use SIMD.
    let mut acc = [0f32; 8];
    let (ca, cb) = (a.chunks_exact(8), b.chunks_exact(8));
    let tail: f32 = ca.remainder().iter().zip(cb.remainder()).map(|(x, y)| x * y).sum();
    for (x, y) in ca.zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    acc.iter().sum::<f32>() + tail
}

pub fn normalize(v: &mut [f32]) {
    let n = dot(v, v).sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

// ---------- automatic grouping ----------

/// Thresholds for [`cluster`], as average cosine similarities between faces.
#[derive(Debug, Clone, Copy)]
pub struct ClusterParams {
    /// A face joins an existing person when it is this similar to their faces on average.
    pub join: f32,
    /// Two groups of ungrouped faces merge when their faces are this similar on average.
    pub merge: f32,
    /// A new person needs this many faces, so one-off strangers don't each become someone.
    pub min_group: usize,
}

impl Default for ClusterParams {
    fn default() -> Self {
        ClusterParams { join: 0.45, merge: 0.50, min_group: 3 }
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ClusterStats {
    /// Faces added to people who already existed.
    pub joined: usize,
    pub new_people: usize,
    /// Faces in new people.
    pub grouped: usize,
    /// Faces still without a person.
    pub ungrouped: usize,
}

struct FaceVec {
    id: i64,
    person: Option<i64>,
    locked: i64,
    v: Vec<f32>,
}

fn add(sum: &mut [f32], v: &[f32]) {
    sum.iter_mut().zip(v).for_each(|(a, b)| *a += b);
}

/// A possible merge of groups `a` and `b` whose faces are `0` similar on average.
/// Ordered by similarity, so a max-heap pops the best merge first.
#[derive(PartialEq)]
struct Candidate(f32, usize, usize);

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

impl Ord for Candidate {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&o.0).then((o.1, o.2).cmp(&(self.1, self.2)))
    }
}

/// Group faces into people with the default thresholds.
pub fn cluster(db: &Db) -> Result<ClusterStats> {
    cluster_with(db, &ClusterParams::default())
}

/// Group faces into people. Faces that have a person stay there. Ungrouped faces join the
/// person they resemble most on average, if close enough; the rest are grouped among
/// themselves by average linkage: the two most similar groups merge as long as their faces
/// are, on average, similar enough. Unlike chaining neighbours (single linkage, DBSCAN),
/// a few look-alike photos can't join two different people.
///
/// Average similarity between groups is cheap: for unit vectors it is the dot product of
/// the groups' summed vectors divided by both sizes.
pub fn cluster_with(db: &Db, params: &ClusterParams) -> Result<ClusterStats> {
    let faces: Vec<FaceVec> = {
        let conn = db.conn()?;
        let mut stmt = conn.prepare("SELECT id, person_id, locked, embedding FROM faces")?;
        stmt.query_map([], |r| {
            Ok(FaceVec { id: r.get(0)?, person: r.get(1)?, locked: r.get(2)?, v: from_blob(&r.get::<_, Vec<u8>>(3)?) })
        })?
        .collect::<std::result::Result<_, _>>()?
    };

    // Summed faces of each person, and how many.
    let mut people: HashMap<i64, (Vec<f32>, f32)> = HashMap::new();
    for f in faces.iter().filter(|f| f.v.len() == FACE_DIM) {
        if let Some(p) = f.person {
            let e = people.entry(p).or_insert_with(|| (vec![0.0; FACE_DIM], 0.0));
            add(&mut e.0, &f.v);
            e.1 += 1.0;
        }
    }

    let free: Vec<&FaceVec> =
        faces.iter().filter(|f| f.person.is_none() && f.locked == AUTO && f.v.len() == FACE_DIM).collect();

    // 1. Join existing people.
    let joins: Vec<Option<i64>> = free
        .par_iter()
        .map(|f| {
            people
                .iter()
                .map(|(p, (sum, n))| (dot(&f.v, sum) / n, *p))
                .filter(|(s, _)| *s >= params.join)
                .max_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, p)| p)
        })
        .collect();
    let mut assign: Vec<(i64, i64)> = Vec::new(); // (face, person)
    let mut rest: Vec<&FaceVec> = Vec::new();
    for (f, j) in free.iter().zip(&joins) {
        match j {
            Some(p) => assign.push((f.id, *p)),
            None => rest.push(f),
        }
    }
    let joined = assign.len();

    // 2. Group the rest. Only pairs similar enough to merge on their own can start a merge.
    let n = rest.len();
    let mut near: Vec<HashSet<usize>> = (0..n)
        .into_par_iter()
        .map(|i| (0..n).filter(|&j| j != i && dot(&rest[i].v, &rest[j].v) >= params.merge).collect())
        .collect();
    let mut sum: Vec<Vec<f32>> = rest.iter().map(|f| f.v.clone()).collect();
    let mut size: Vec<f32> = vec![1.0; n];
    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut alive: Vec<bool> = vec![true; n];
    let avg = |sum: &[Vec<f32>], size: &[f32], a: usize, b: usize| dot(&sum[a], &sum[b]) / (size[a] * size[b]);

    let mut heap = BinaryHeap::new();
    for (a, close) in near.iter().enumerate() {
        for &b in close.iter().filter(|&&b| b > a) {
            heap.push(Candidate(avg(&sum, &size, a, b), a, b));
        }
    }
    while let Some(Candidate(s, a, b)) = heap.pop() {
        if s < params.merge {
            break;
        }
        if !alive[a] || !alive[b] {
            continue;
        }
        let now = avg(&sum, &size, a, b);
        if (now - s).abs() > 1e-6 {
            // One of the groups grew since; queue it again at its current similarity.
            heap.push(Candidate(now, a, b));
            continue;
        }
        // Merge b into a.
        alive[b] = false;
        let (sb, mb, nb) = (std::mem::take(&mut sum[b]), std::mem::take(&mut members[b]), std::mem::take(&mut near[b]));
        add(&mut sum[a], &sb);
        size[a] += size[b];
        members[a].extend(mb);
        near[a].extend(nb);
        near[a].retain(|&c| c != a && c != b && alive[c]);
        let neighbours: Vec<usize> = near[a].iter().copied().collect();
        for c in neighbours {
            near[c].remove(&b);
            near[c].insert(a);
            heap.push(Candidate(avg(&sum, &size, a, c), a.min(c), a.max(c)));
        }
    }
    let mut groups: Vec<Vec<i64>> = (0..n)
        .filter(|&i| alive[i] && members[i].len() >= params.min_group)
        .map(|i| members[i].iter().map(|&m| rest[m].id).collect())
        .collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.len()));
    let grouped: usize = groups.iter().map(Vec::len).sum();

    // Save. The guards skip faces the user changed while this ran.
    let mut conn = db.conn()?;
    let tx = conn.transaction()?;
    {
        let mut set = tx.prepare_cached(
            "UPDATE faces SET person_id = ?2 WHERE id = ?1 AND person_id IS NULL AND locked = 0
             AND EXISTS (SELECT 1 FROM persons WHERE id = ?2)",
        )?;
        for (face, person) in &assign {
            set.execute(params![face, person])?;
        }
        let now = Utc::now().to_rfc3339();
        for g in &groups {
            tx.execute("INSERT INTO persons (created_at) VALUES (?1)", [&now])?;
            let person = tx.last_insert_rowid();
            for face in g {
                set.execute(params![face, person])?;
            }
        }
        // People left without faces (all removed, merged or deleted with their files).
        tx.execute(
            "DELETE FROM persons WHERE NOT EXISTS (SELECT 1 FROM faces WHERE person_id = persons.id)",
            [],
        )?;
    }
    tx.commit()?;
    Ok(ClusterStats { joined, new_people: groups.len(), grouped, ungrouped: n - grouped })
}

/// How many faces have no person and could still get one.
pub fn ungrouped_count(db: &Db) -> Result<u64> {
    let n: i64 = db
        .conn()?
        .query_row("SELECT count(*) FROM faces WHERE person_id IS NULL AND locked = 0", [], |r| r.get(0))?;
    Ok(n as u64)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::test_db;

    /// A unit vector near `base` (a "person"), nudged by `seed`.
    pub fn face_like(base: usize, seed: u64) -> Vec<f32> {
        let mut v = vec![0f32; FACE_DIM];
        v[base] = 1.0;
        let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        for e in v.iter_mut() {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *e += ((x >> 33) as f32 / u32::MAX as f32 - 0.25) * 0.12;
        }
        normalize(&mut v);
        v
    }

    pub fn insert_media(db: &Db, n: usize) -> Vec<i64> {
        let conn = db.conn().unwrap();
        conn.execute("INSERT INTO folders (path, added_at) VALUES ('C:/p', 'now')", []).unwrap();
        (0..n)
            .map(|i| {
                conn.execute(
                    "INSERT INTO media (folder_id, path, name, size, mtime, kind, mime, taken_at, stage, added_at)
                     VALUES (1, ?1, ?2, 1, 1, 'photo', 'image/jpeg', '2024-01-01T00:00:00', 4, 'now')",
                    params![format!("C:/p/{i}.jpg"), format!("{i}.jpg")],
                )
                .unwrap();
                conn.last_insert_rowid()
            })
            .collect()
    }

    pub fn insert_face(db: &Db, media: i64, v: &[f32]) -> i64 {
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO faces (media_id, x, y, w, h, score, embedding) VALUES (?1, 0.1, 0.1, 0.2, 0.2, 0.9, ?2)",
            params![media, to_blob(v)],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn person_of(db: &Db, face: i64) -> Option<i64> {
        db.conn().unwrap().query_row("SELECT person_id FROM faces WHERE id = ?1", [face], |r| r.get(0)).unwrap()
    }

    #[test]
    fn dot_matches_naive() {
        let a: Vec<f32> = (0..131).map(|i| i as f32 * 0.01).collect();
        let b: Vec<f32> = (0..131).map(|i| 1.0 - i as f32 * 0.003).collect();
        let naive: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        assert!((dot(&a, &b) - naive).abs() < 1e-3);
        assert_eq!(from_blob(&to_blob(&a)), a);
    }

    #[test]
    fn groups_faces_and_respects_the_user() {
        let (_dir, db) = test_db();
        let media = insert_media(&db, 12);
        // Person A in 5 photos, person B in 4, and two strangers once each.
        let a: Vec<i64> = (0..5).map(|i| insert_face(&db, media[i], &face_like(0, i as u64))).collect();
        let b: Vec<i64> = (0..4).map(|i| insert_face(&db, media[5 + i], &face_like(1, 10 + i as u64))).collect();
        let s1 = insert_face(&db, media[9], &face_like(2, 99));
        let s2 = insert_face(&db, media[10], &face_like(3, 98));

        let stats = cluster(&db).unwrap();
        assert_eq!((stats.new_people, stats.grouped, stats.ungrouped), (2, 9, 2), "{stats:?}");
        let pa = person_of(&db, a[0]).unwrap();
        let pb = person_of(&db, b[0]).unwrap();
        assert_ne!(pa, pb);
        assert!(a.iter().all(|f| person_of(&db, *f) == Some(pa)));
        assert!(b.iter().all(|f| person_of(&db, *f) == Some(pb)));
        assert_eq!((person_of(&db, s1), person_of(&db, s2)), (None, None));

        let people = list(&db, false).unwrap();
        assert_eq!(people.iter().map(|p| p.count).collect::<Vec<_>>(), [5, 4]);
        assert!(people[0].cover_face.is_some());

        // A new photo of A joins A.
        let a6 = insert_face(&db, media[11], &face_like(0, 50));
        assert_eq!(cluster(&db).unwrap().joined, 1);
        assert_eq!(person_of(&db, a6), Some(pa));

        // "Not this person" sticks.
        set_face_person(&db, a6, None).unwrap();
        cluster(&db).unwrap();
        assert_eq!(person_of(&db, a6), None);

        // Naming, search by name, hiding, merging.
        rename(&db, pa, Some("  Anna Roy ")).unwrap();
        assert_eq!(get(&db, pa).unwrap().name.as_deref(), Some("Anna Roy"));
        assert_eq!(media_named(&db, "ann").unwrap().len(), 5);
        assert_eq!(media_named(&db, "roy anna").unwrap().len(), 5);
        assert!(media_named(&db, "bob").unwrap().is_empty());
        set_hidden(&db, pb, true).unwrap();
        assert_eq!(list(&db, false).unwrap().len(), 1);
        assert_eq!(list(&db, true).unwrap().len(), 2);
        let merged = merge(&db, &[pb], pa).unwrap();
        assert_eq!((merged.count, merged.name.as_deref()), (9, Some("Anna Roy")));
        assert!(matches!(get(&db, pb), Err(Error::NotFound)));
        // Merged faces are the user's choice now.
        let locked: i64 = db.conn().unwrap().query_row("SELECT locked FROM faces WHERE id = ?1", [b[0]], |r| r.get(0)).unwrap();
        assert_eq!(locked, BY_USER);

        // A face given its own person; the person disappears when its face goes.
        let p = new_person_for_face(&db, s1, Some("Sam")).unwrap();
        assert_eq!((p.count, p.name.as_deref()), (1, Some("Sam")));
        set_face_person(&db, s1, None).unwrap();
        cluster(&db).unwrap();
        assert!(matches!(get(&db, p.id), Err(Error::NotFound)));

        assert!(rename(&db, pa, Some(&"x".repeat(101))).is_err());
        assert!(matches!(set_face_person(&db, 9999, None), Err(Error::NotFound)));
    }
}
