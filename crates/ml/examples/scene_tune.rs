//! How scenes come out on a real library, for tuning `scene::TEXTS`, `GUARDS` and `MARGIN`:
//! `cargo run -p photos-ml --example scene_tune -- <models> <data dir> <out dir>`
//! Prints how many photos each scene gets and how many changed from the old texts, and for
//! each narrow scene writes contact sheets of the photos it leads, bucketed by its lead.
//! Set TEXT_SHEETS to also write the photos each text matches best.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use image::{GenericImage, RgbImage, imageops};
use photos_core::edit::filters::Scene;
use photos_ml::clip::TextEncoder;
use photos_ml::scene;
use photos_ml::{CLIP_TEXT, CLIP_TOKENIZER, open_session};

const BUCKETS: &[f32] = &[-1.0, 0.0, 0.005, 0.01, 0.015, 0.02, 0.03, 1.0];
const TILE: u32 = 150;
const PER_SHEET: usize = 48;

/// The texts before the narrow kinds, to see what changed.
const OLD: &[(Scene, &str)] = &[
    (Scene::Portrait, "a portrait of a person"),
    (Scene::Portrait, "a selfie"),
    (Scene::Portrait, "a group of people"),
    (Scene::Landscape, "a landscape with mountains"),
    (Scene::Landscape, "a beach and the sea"),
    (Scene::Landscape, "nature with trees and sky"),
    (Scene::Food, "food on a plate"),
    (Scene::Food, "a meal in a restaurant"),
    (Scene::Night, "a city at night with lights"),
    (Scene::Night, "a dark scene at night"),
    (Scene::City, "a city street with buildings"),
    (Scene::City, "architecture"),
    (Scene::Other, "an object on a table"),
    (Scene::Other, "a pet animal"),
    (Scene::Document, "a document or a screenshot"),
    (Scene::Document, "a page of a book with printed text"),
    (Scene::Document, "handwritten notes on paper"),
    (Scene::Document, "a receipt or a printed form"),
    (Scene::Document, "a whiteboard or a slide with text"),
];

fn thumb(data: &Path, id: i64) -> Option<RgbImage> {
    let p = data.join("cache").join("thumb").join((id / 1000).to_string()).join(format!("{id}.jpg"));
    let img = image::open(p).ok()?.to_rgb8();
    Some(imageops::thumbnail(&img, TILE, TILE))
}

/// Up to [`PER_SHEET`] of `ids`, spread over the list, as one picture.
fn sheet(data: &Path, ids: &[i64], out: &Path) {
    let step = (ids.len() as f32 / PER_SHEET as f32).max(1.0);
    let ids: Vec<i64> = (0..ids.len().min(PER_SHEET)).map(|k| ids[(k as f32 * step) as usize]).collect();
    let cols = 8u32;
    let rows = (ids.len() as u32).div_ceil(cols).max(1);
    let mut s = RgbImage::from_pixel(cols * TILE, rows * TILE, image::Rgb([30, 30, 30]));
    for (k, id) in ids.iter().enumerate() {
        if let Some(t) = thumb(data, *id) {
            let (x, y) = ((k as u32 % cols) * TILE, (k as u32 / cols) * TILE);
            let _ = s.copy_from(&t, x + (TILE - t.width()) / 2, y + (TILE - t.height()) / 2);
        }
    }
    s.save(out).unwrap();
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (models, data, out) = (PathBuf::from(&a[0]), PathBuf::from(&a[1]), PathBuf::from(&a[2]));
    std::fs::create_dir_all(&out).unwrap();
    let db = photos_core::Db::open(&data.join("library.db")).unwrap();
    let (s, _) = open_session(&models.join(CLIP_TEXT), false, &[]).unwrap();
    let mut text = TextEncoder::new(s, &models.join(CLIP_TOKENIZER)).unwrap();
    let names: Vec<&str> = scene::all_texts().collect();
    let texts: Vec<Vec<f32>> = names.iter().map(|t| text.embed(t).unwrap()).collect();
    let old: Vec<Vec<f32>> = OLD.iter().map(|(_, t)| text.embed(t).unwrap()).collect();
    let conn = db.conn().unwrap();
    let faces: HashSet<i64> =
        conn.prepare("SELECT DISTINCT media_id FROM faces").unwrap().query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect();
    let rows: Vec<(i64, Vec<f32>)> = conn
        .prepare("SELECT c.media_id, c.vec FROM clip_embeddings c JOIN media m ON m.id = c.media_id WHERE m.kind = 'photo' AND m.trashed_at IS NULL")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, photos_core::people::from_blob(&r.get::<_, Vec<u8>>(1)?))))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();

    let mut picked: HashMap<Scene, usize> = HashMap::new();
    let mut members: HashMap<Scene, Vec<i64>> = HashMap::new();
    let mut changed: HashMap<(Scene, Scene), Vec<i64>> = HashMap::new();
    let mut by_text: HashMap<usize, Vec<i64>> = HashMap::new();
    let mut leads: HashMap<Scene, Vec<(f32, i64)>> = HashMap::new();
    for (id, v) in &rows {
        let people = faces.contains(id);
        let scores: Vec<f32> = texts.iter().map(|t| photos_core::people::dot(v, t)).collect();
        let now = scene::pick(&scores, people).unwrap();
        *picked.entry(now).or_default() += 1;
        members.entry(now).or_default().push(*id);
        let before = OLD
            .iter()
            .zip(&old)
            .filter(|((s, _), _)| people || *s != Scene::Portrait)
            .map(|((s, _), t)| (*s, photos_core::people::dot(v, t)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap()
            .0;
        if before != now {
            changed.entry((before, now)).or_default().push(*id);
        }
        let top = (0..names.len())
            .filter(|k| people || *k >= scene::TEXTS.len() || scene::TEXTS[*k].0 != Scene::Portrait)
            .max_by(|a, b| scores[*a].total_cmp(&scores[*b]))
            .unwrap();
        by_text.entry(top).or_default().push(*id);
        if let Some((s, lead)) = scene::narrow_lead(&scores, people) {
            leads.entry(s).or_default().push((lead, *id));
        }
    }
    println!("{} photos, margin {}", rows.len(), scene::MARGIN);
    for s in Scene::ALL {
        let ids = members.get(&s).cloned().unwrap_or_default();
        let step = (ids.len() / 12).max(1);
        let sample: Vec<String> = ids.iter().step_by(step).take(12).map(|i| i.to_string()).collect();
        println!("  {s:?}: {} (e.g. {})", picked.get(&s).copied().unwrap_or(0), sample.join(","));
    }
    println!("changed from the old texts:");
    let mut changes: Vec<_> = changed.iter().collect();
    changes.sort_by_key(|c| std::cmp::Reverse(c.1.len()));
    for ((b, n), ids) in changes {
        println!("  {b:?} -> {n:?}: {}", ids.len());
        sheet(&data, ids, &out.join(format!("changed-{b:?}-{n:?}.jpg")));
    }
    println!("best text:");
    for (k, t) in names.iter().enumerate() {
        let ids = by_text.get(&k).cloned().unwrap_or_default();
        println!("  {:>5}  {t}", ids.len());
        if std::env::var_os("TEXT_SHEETS").is_some() && !ids.is_empty() {
            sheet(&data, &ids, &out.join(format!("text-{k:02}.jpg")));
        }
    }
    for (s, list) in &mut leads {
        list.sort_by(|a, b| a.0.total_cmp(&b.0));
        println!("{s:?} leads the kinds {} times", list.len());
        for w in BUCKETS.windows(2) {
            let ids: Vec<i64> = list.iter().filter(|m| m.0 >= w[0] && m.0 < w[1]).map(|m| m.1).collect();
            if !ids.is_empty() {
                println!("  lead {:.3}..{:.3}: {}", w[0], w[1], ids.len());
                sheet(&data, &ids, &out.join(format!("{s:?}-{:.3}-{:.3}.jpg", w[0], w[1])));
            }
        }
    }
}
