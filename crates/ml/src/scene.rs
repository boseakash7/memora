//! What kind of scene a photo shows, from its CLIP vector: the vector is compared with texts
//! describing each kind (zero-shot), for suggesting filters and tuning Enhance.
//!
//! The broad kinds (people, landscape, food, ...) are simply the best-matching text. The
//! narrow kinds (sunset, animals, flowers, snow) also look like broad ones, so they have to
//! win clearly, over the broad texts and over [`GUARDS`], their usual look-alikes.

use photos_core::edit::filters::Scene;

/// Texts that describe each kind of scene.
pub const TEXTS: &[(Scene, &str)] = &[
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
    (Scene::Document, "a document or a screenshot"),
    (Scene::Document, "a page of a book with printed text"),
    (Scene::Document, "handwritten notes on paper"),
    (Scene::Document, "a receipt or a printed form"),
    (Scene::Document, "a whiteboard or a slide with text"),
    (Scene::Sunset, "a colourful orange and pink sunset sky"),
    (Scene::Sunset, "the sun setting over the horizon"),
    (Scene::Pet, "a dog"),
    (Scene::Pet, "a cat"),
    (Scene::Pet, "a wild animal or a bird"),
    (Scene::Flowers, "a close-up of a flower"),
    (Scene::Flowers, "a bouquet of flowers"),
    (Scene::Flowers, "a garden full of flowers"),
    (Scene::Snow, "a mountain covered in white snow"),
    (Scene::Snow, "people standing in the snow"),
];

/// What the narrow kinds are most often confused with. A guard that matches better keeps a
/// narrow kind from being chosen, but never decides the kind itself.
pub const GUARDS: &[&str] = &[
    // Not snow or a sunset.
    "a foggy, misty landscape",
    "a grey overcast sky over water",
    "the moon in the sky",
    // Not flowers.
    "green leaves of a plant",
    // Not animals. Faces are not always found (asleep, turned away), and then the people
    // texts are left out.
    "part of a person's body, close up",
    "a wristwatch",
    "a scooter or a car",
    "a room in a house",
];

/// Every text, in the order [`pick`] expects their scores: [`TEXTS`], then [`GUARDS`].
pub fn all_texts() -> impl Iterator<Item = &'static str> {
    TEXTS.iter().map(|t| t.1).chain(GUARDS.iter().copied())
}

pub fn narrow(s: Scene) -> bool {
    matches!(s, Scene::Sunset | Scene::Pet | Scene::Flowers | Scene::Snow)
}

/// How much better (in cosine similarity) a narrow kind has to match than the best broad
/// text or guard. Tuned on a real library with `examples/scene_tune.rs`: above it nearly
/// every photo was right, below it animals and sunsets were wrong about half the time.
pub const MARGIN: f32 = 0.01;

/// Each kind's best match among the scores of [`TEXTS`], best first.
pub fn by_kind(scores: &[f32], people_in_it: bool) -> Vec<(Scene, f32)> {
    let mut best: Vec<(Scene, f32)> = Vec::new();
    for ((scene, _), s) in TEXTS.iter().zip(scores) {
        // Faces are looked for in the same pass that stored the vector, so with none found
        // there is nobody in the picture, whatever CLIP thinks.
        if *scene == Scene::Portrait && !people_in_it {
            continue;
        }
        match best.iter_mut().find(|b| b.0 == *scene) {
            Some(b) => b.1 = b.1.max(*s),
            None => best.push((*scene, *s)),
        }
    }
    best.sort_by(|a, b| b.1.total_cmp(&a.1));
    best
}

/// How far the best narrow kind beats everything else (broad kinds and guards), if one
/// matches best of all the kinds.
pub fn narrow_lead(scores: &[f32], people_in_it: bool) -> Option<(Scene, f32)> {
    let kinds = by_kind(scores, people_in_it);
    let first = *kinds.first()?;
    if !narrow(first.0) {
        return None;
    }
    let broad = kinds.iter().find(|k| !narrow(k.0)).map_or(f32::MIN, |k| k.1);
    let guard = scores.get(TEXTS.len()..).unwrap_or(&[]).iter().copied().fold(f32::MIN, f32::max);
    Some((first.0, first.1 - broad.max(guard)))
}

/// The kind of scene, from each text's similarity to the photo (see [`all_texts`]).
pub fn pick(scores: &[f32], people_in_it: bool) -> Option<Scene> {
    if let Some((scene, lead)) = narrow_lead(scores, people_in_it)
        && lead >= MARGIN
    {
        return Some(scene);
    }
    by_kind(scores, people_in_it).into_iter().find(|k| !narrow(k.0)).map(|k| k.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scores(wins: &[(Scene, f32)], guard: f32) -> Vec<f32> {
        TEXTS
            .iter()
            .map(|(s, _)| wins.iter().find(|w| w.0 == *s).map_or(0.1, |w| w.1))
            .chain(GUARDS.iter().map(|_| guard))
            .collect()
    }

    #[test]
    fn every_kind_has_texts() {
        for s in Scene::ALL {
            assert!(TEXTS.iter().any(|t| t.0 == s), "{s:?}");
        }
    }

    #[test]
    fn narrow_kinds_need_a_clear_win() {
        let close = scores(&[(Scene::Sunset, 0.245), (Scene::Landscape, 0.24)], 0.0);
        assert_eq!(pick(&close, false), Some(Scene::Landscape));
        let clear = scores(&[(Scene::Sunset, 0.27), (Scene::Landscape, 0.24)], 0.0);
        assert_eq!(pick(&clear, false), Some(Scene::Sunset));
    }

    #[test]
    fn guards_stop_narrow_kinds_but_do_not_decide() {
        let s = scores(&[(Scene::Snow, 0.27), (Scene::Landscape, 0.24)], 0.265);
        assert_eq!(pick(&s, false), Some(Scene::Landscape));
        // A guard matching best of all leaves the broad answer as it was.
        let s = scores(&[(Scene::Portrait, 0.25), (Scene::City, 0.2)], 0.3);
        assert_eq!(pick(&s, true), Some(Scene::Portrait));
    }

    #[test]
    fn no_faces_no_portrait() {
        let s = scores(&[(Scene::Portrait, 0.3), (Scene::City, 0.2)], 0.0);
        assert_eq!(pick(&s, false), Some(Scene::City));
        assert_eq!(pick(&s, true), Some(Scene::Portrait));
    }
}
