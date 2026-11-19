//! Enhance: slider values worked out from the picture itself (its brightness spread,
//! clipped areas, colour cast and colourfulness), tuned by what the scene is when recognition
//! knows it. The result is ordinary slider values, so it can be adjusted afterwards.

use image::RgbImage;

use super::color::{self, Rgb};
use super::filters::Scene;
use super::render::{sample_frame, white_balance};
use super::{Adjust, Edit};

pub fn enhance(src: &RgbImage, edit: &Edit, scene: Option<Scene>) -> Adjust {
    // Statistics of the frame as cropped, before any colour change.
    let frame = Edit { adjust: Adjust::default(), filter: None, blur: 0.0, ..edit.clone() };
    let (pixels, w, h) = sample_frame(src, &frame, 256);
    adjust_for(&pixels, w, h, scene)
}

fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.5;
    }
    sorted[((sorted.len() - 1) as f32 * p).round() as usize]
}

fn round(v: f32) -> f32 {
    v.round()
}

pub(crate) fn adjust_for(pixels: &[Rgb], w: u32, h: u32, scene: Option<Scene>) -> Adjust {
    let mut a = Adjust::default();
    if pixels.is_empty() {
        return a;
    }
    let mut lum: Vec<f32> = pixels.iter().map(|c| color::perceive(color::luma(*c))).collect();
    lum.sort_by(f32::total_cmp);
    // Exposure: how bright the photo is where the subject usually is (centre-weighted), and
    // a change only when that is clearly too dark or too bright. A bright sky or a dark
    // background alone is not a reason to change a well-exposed photo.
    let (mut sum, mut weights) = (0.0f32, 0.0f32);
    for (k, c) in pixels.iter().enumerate() {
        let (x, y) = ((k as u32 % w.max(1)) as f32 / w.max(1) as f32 - 0.5, (k as u32 / w.max(1)) as f32 / h.max(1) as f32 - 0.5);
        let wgt = (-(x * x + y * y) / (2.0 * 0.3 * 0.3)).exp();
        sum += color::perceive(color::luma(*c)) * wgt;
        weights += wgt;
    }
    let key = if weights > 0.0 { sum / weights } else { 0.5 };
    let (lo, hi) = match scene {
        Some(Scene::Night) => (0.2, 0.45),
        // The sky is the subject and is meant to be dim and glowing.
        Some(Scene::Sunset) => (0.25, 0.55),
        // Snow is meant to be bright; brought down to a usual middle it turns grey.
        Some(Scene::Snow) => (0.5, 0.75),
        _ => (0.36, 0.62),
    };
    let edge = key.clamp(lo, hi);
    let ev = ((color::unperceive(edge) / color::unperceive(key.max(0.02))).log2() * 0.7).clamp(-0.8, 1.5);
    a.exposure = round(ev * 50.0);
    let gain = 2f32.powf(a.exposure / 50.0);
    let after: Vec<f32> = lum.iter().map(|l| color::perceive(color::unperceive(*l) * gain)).collect();
    let (p005, p05, p95, p995) =
        (percentile(&after, 0.005), percentile(&after, 0.05), percentile(&after, 0.95), percentile(&after, 0.995));
    let n = after.len() as f32;
    let bright = after.iter().filter(|l| **l > 0.95).count() as f32 / n;
    let dark = after.iter().filter(|l| **l < 0.12).count() as f32 / n;

    if p995 < 0.9 {
        a.whites = round(((0.95 - p995) * 250.0).min(40.0));
    }
    // Lifted blacks in a photo that is not simply dark: it looks faded.
    if p005 > 0.05 && percentile(&after, 0.5) > 0.3 {
        a.blacks = -round(((p005 - 0.02) * 400.0).min(50.0));
    }
    if bright > 0.01 {
        // Snow fills much of the frame with near-white; pulling all of it down greys it.
        let most = if scene == Some(Scene::Snow) { 25.0 } else { 60.0 };
        a.highlights = -round((bright * 600.0).clamp(15.0, most));
    }
    if dark > 0.15 && scene != Some(Scene::Night) {
        a.shadows = round((dark * 120.0).min(45.0));
    }
    let spread = p95 - p05;
    a.contrast = if spread < 0.65 { round(((0.7 - spread) * 80.0).min(25.0)) } else { 5.0 };

    // Colour cast: the average of near-neutral midtones, corrected part of the way.
    let (mut sa, mut sb, mut count) = (0.0f32, 0.0f32, 0usize);
    let mut chroma_sum = 0.0f32;
    for c in pixels {
        // As it will look after the exposure change.
        let lab = color::to_oklab([c[0] * gain, c[1] * gain, c[2] * gain]);
        let chroma = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
        chroma_sum += chroma;
        if chroma < 0.08 && (0.25..0.92).contains(&lab[0]) {
            sa += lab[1];
            sb += lab[2];
            count += 1;
        }
    }
    if count as f32 > n * 0.03 {
        let (ma, mb) = (sa / count as f32, sb / count as f32);
        let share = match scene {
            // Warm food and night lights are usually meant.
            Some(Scene::Food) | Some(Scene::Night) => 0.3,
            // A sunset's colour is the point of it.
            Some(Scene::Sunset) => 0.15,
            // Paper and screens are meant to be white, and so is snow (often blue in shade).
            Some(Scene::Document) => 0.8,
            Some(Scene::Snow) => 0.7,
            _ => 0.5,
        };
        // How far one unit of each slider moves a grey (finite differences).
        let grey = |t: f32, m: f32| {
            let wb = white_balance(t, m);
            color::to_oklab([0.2 * wb[0], 0.2 * wb[1], 0.2 * wb[2]])
        };
        let g0 = grey(0.0, 0.0);
        let db_dt = (grey(0.1, 0.0)[2] - g0[2]) / 0.1;
        let da_dm = (grey(0.0, 0.1)[1] - g0[1]) / 0.1;
        if db_dt.abs() > 1e-4 {
            a.temperature = round((-mb * share / db_dt * 100.0).clamp(-30.0, 30.0));
        }
        if da_dm.abs() > 1e-4 {
            a.tint = round((-ma * share / da_dm * 100.0).clamp(-15.0, 15.0));
        }
    }

    let mean_chroma = chroma_sum / n;
    a.vibrance = if mean_chroma < 0.04 {
        30.0
    } else if mean_chroma < 0.08 {
        20.0
    } else {
        10.0
    };
    match scene {
        Some(Scene::Portrait) => {
            a.vibrance = a.vibrance.min(15.0);
        }
        Some(Scene::Landscape) => {
            a.vibrance += 10.0;
            a.clarity = 12.0;
            if spread < 0.6 {
                a.dehaze = 10.0;
            }
        }
        Some(Scene::City) => a.clarity = 10.0,
        Some(Scene::Sunset) => a.vibrance += 5.0,
        // Fur and feathers.
        Some(Scene::Pet) => a.clarity = 10.0,
        // Petals are already saturated and clip first.
        Some(Scene::Flowers) => {
            a.vibrance = a.vibrance.min(10.0);
            a.clarity = 4.0;
        }
        Some(Scene::Snow) => {
            a.vibrance = a.vibrance.min(10.0);
            a.clarity = 8.0;
        }
        // Crisp, legible text; no extra colour.
        Some(Scene::Document) => {
            a.vibrance = 0.0;
            a.clarity = 15.0;
        }
        _ => a.clarity = 6.0,
    }
    a
}
