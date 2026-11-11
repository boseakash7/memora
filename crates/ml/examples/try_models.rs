//! Run the recognition models on some JPEG/PNG files: timings, faces (drawn into copies in
//! `<out dir>`), face similarities and CLIP scores for a few words.
//!
//! `cargo run -p photos-ml --release --example try_models -- <models dir> <out dir> <cpu|gpu> <images>...`

use std::path::PathBuf;
use std::time::Instant;

use photos_ml::clip::{ImageEncoder, TextEncoder};
use photos_ml::faces::{Detector, Embedder};
use photos_ml::{CLIP_TEXT, CLIP_TOKENIZER, CLIP_VISION, SFACE, YUNET, open_session};

fn main() -> photos_ml::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (models, out, gpu) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]), args[2] == "gpu");
    std::fs::create_dir_all(&out)?;
    let t = Instant::now();
    let (d, dev) = open_session(&models.join(YUNET), gpu, &[])?;
    let (e, _) = open_session(&models.join(SFACE), gpu, &[])?;
    let sizes = [("batch_size", 1), ("num_channels", 3), ("height", 224), ("width", 224)];
    let (c, _) = open_session(&models.join(CLIP_VISION), gpu, &sizes)?;
    let (tx, _) = open_session(&models.join(CLIP_TEXT), false, &[])?;
    println!("loaded on {dev:?} in {:.1?}", t.elapsed());
    let (mut det, mut emb, mut clip) = (Detector::new(d), Embedder::new(e), ImageEncoder::new(c));
    let mut text = TextEncoder::new(tx, &models.join(CLIP_TOKENIZER))?;

    let words = ["a person", "a group of people", "a dog", "a beach", "mountains", "food", "a car", "a building", "a selfie", "a document"];
    let wv: Vec<Vec<f32>> = words.iter().map(|w| text.embed(w)).collect::<Result<_, _>>()?;

    let mut all_faces: Vec<(String, Vec<f32>)> = Vec::new();
    for path in &args[3..] {
        let img = image::open(path).map_err(|e| photos_ml::Error::Model(e.to_string()))?;
        let img = img.thumbnail(1600, 1600).to_rgb8();
        let name = std::path::Path::new(path).file_stem().unwrap().to_string_lossy().to_string();
        let t = Instant::now();
        let p = photos_ml::prepare(img.clone())?;
        let t_prep = t.elapsed();
        let t = Instant::now();
        let faces = det.detect(&p.faces)?;
        let t_det = t.elapsed();
        let t = Instant::now();
        let mut annotated = img.clone();
        for (i, f) in faces.iter().enumerate() {
            all_faces.push((format!("{name}#{i}"), emb.embed(&img, f)?));
        }
        let t_emb = t.elapsed();
        for f in &faces {
            let [x, y, w, h] = f.rect.map(|v| v as u32);
            for px in x..(x + w).min(img.width()) {
                for py in [y, (y + h).min(img.height() - 1)] {
                    annotated.put_pixel(px, py, image::Rgb([255, 0, 0]));
                }
            }
            for py in y..(y + h).min(img.height()) {
                for px in [x, (x + w).min(img.width() - 1)] {
                    annotated.put_pixel(px, py, image::Rgb([255, 0, 0]));
                }
            }
            for l in f.landmarks {
                let (lx, ly) = (l[0] as u32, l[1] as u32);
                if lx + 2 < img.width() && ly + 2 < img.height() {
                    for dx in 0..3 {
                        for dy in 0..3 {
                            annotated.put_pixel(lx + dx, ly + dy, image::Rgb([0, 255, 0]));
                        }
                    }
                }
            }
        }
        let t = Instant::now();
        let v = clip.embed(p.clip)?;
        let t_clip = t.elapsed();
        let mut scores: Vec<(f32, &str)> =
            wv.iter().zip(words).map(|(w, n)| (photos_core::people::dot(w, &v), n)).collect();
        scores.sort_by(|a, b| b.0.total_cmp(&a.0));
        println!(
            "{name}: {} faces {:?} | prepare {t_prep:.1?} detect {t_det:.1?} embed {t_emb:.1?} clip {t_clip:.1?} | {}",
            faces.len(),
            faces.iter().map(|f| (f.score * 100.0) as u32).collect::<Vec<_>>(),
            scores.iter().take(3).map(|(s, n)| format!("{n} {s:.3}")).collect::<Vec<_>>().join(", ")
        );
        annotated.save(out.join(format!("{name}.jpg"))).map_err(|e| photos_ml::Error::Model(e.to_string()))?;
    }
    println!("\nmost similar face pairs:");
    let mut pairs = Vec::new();
    for i in 0..all_faces.len() {
        for j in i + 1..all_faces.len() {
            pairs.push((photos_core::people::dot(&all_faces[i].1, &all_faces[j].1), i, j));
        }
    }
    pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (s, i, j) in pairs.iter().take(25) {
        println!("  {s:.3} {} {}", all_faces[*i].0, all_faces[*j].0);
    }
    Ok(())
}
