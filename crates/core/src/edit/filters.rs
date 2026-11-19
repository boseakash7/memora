//! The filter catalog. Every filter is a set of parameters for one look stage (see
//! `render`): tone curves in display space, Oklab hue bands, split toning, a black-and-white
//! mix, halation glow, grain and vignette. Nothing is a bitmap LUT, so filters cost nothing
//! to ship, work at any strength, and render the same at every size.
//!
//! The film looks are our own, made from what photographers describe of those stocks:
//! Portra's warm, soft skin and muted greens, 400H's airy pastels, Ektar's and Velvia's
//! saturation, Fujifilm's Classic Chrome, Classic Negative and Nostalgic Negative, CineStill
//! 800T's tungsten cast and red halation, Tri-X's grain. Names avoid the trademarks.

use serde::Serialize;

/// Oklab hue angles (degrees) of the colours bands are aimed at.
pub mod hue {
    pub const RED: f32 = 29.0;
    pub const ORANGE: f32 = 58.0;
    pub const YELLOW: f32 = 105.0;
    pub const GREEN: f32 = 142.0;
    pub const AQUA: f32 = 195.0;
    pub const BLUE: f32 = 250.0;
    pub const PURPLE: f32 = 300.0;
    pub const MAGENTA: f32 = 330.0;
    /// Teal for shadows in split toning.
    pub const TEAL: f32 = 215.0;
    pub const AMBER: f32 = 75.0;
}

/// A change to the colours around one hue.
#[derive(Debug, Clone, Copy)]
pub struct Band {
    /// Oklab hue the band is centred on, degrees.
    pub hue: f32,
    /// Hue rotation, degrees (positive goes red → yellow → green → blue).
    pub shift: f32,
    /// Chroma change: 0.2 means 20 % more.
    pub sat: f32,
    /// Lightness change in Oklab L, scaled by how colourful the pixel is.
    pub light: f32,
}

const fn band(hue: f32, shift: f32, sat: f32, light: f32) -> Band {
    Band { hue, shift, sat, light }
}

/// A tint: an Oklab hue (degrees) and an amount in Oklab chroma (0.03 is clearly visible).
#[derive(Debug, Clone, Copy, Default)]
pub struct Tint {
    pub hue: f32,
    pub amount: f32,
}

const fn tint(hue: f32, amount: f32) -> Tint {
    Tint { hue, amount }
}

const NO_TINT: Tint = tint(0.0, 0.0);

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Group {
    Everyday,
    Film,
    Cinematic,
    Vintage,
    Mono,
}

/// One filter.
#[derive(Debug, Clone)]
pub struct Look {
    pub id: &'static str,
    pub name: &'static str,
    pub group: Group,
    pub about: &'static str,
    /// Tone curves on sRGB-encoded values: all channels, then red, green, blue.
    /// An empty slice leaves the channel alone.
    pub curve: [&'static [(f32, f32)]; 4],
    /// White balance shift, the same scale as the Temperature/Tint sliders (-1..1).
    pub warmth: f32,
    pub tint: f32,
    /// Overall chroma change (0.2 = 20 % more) and vibrance (boosts muted colours most).
    pub sat: f32,
    pub vib: f32,
    pub bands: &'static [Band],
    pub shadows: Tint,
    pub highlights: Tint,
    /// Black and white: weights for linear R, G, B (sum about 1).
    pub mono: Option<[f32; 3]>,
    /// Red glow around bright areas, like light scattering in film (0..1).
    pub glow: f32,
    pub grain: f32,
    /// Negative darkens the corners.
    pub vignette: f32,
}

const NONE: &[(f32, f32)] = &[];

const BASE: Look = Look {
    id: "",
    name: "",
    group: Group::Everyday,
    about: "",
    curve: [NONE, NONE, NONE, NONE],
    warmth: 0.0,
    tint: 0.0,
    sat: 0.0,
    vib: 0.0,
    bands: &[],
    shadows: NO_TINT,
    highlights: NO_TINT,
    mono: None,
    glow: 0.0,
    grain: 0.0,
    vignette: 0.0,
};

use hue::*;

pub const LOOKS: &[Look] = &[
    // ---------- everyday ----------
    Look {
        id: "vivid",
        name: "Vivid",
        about: "More contrast and colour, for any photo",
        curve: [&[(0.0, 0.0), (0.25, 0.21), (0.75, 0.8), (1.0, 1.0)], NONE, NONE, NONE],
        sat: 0.12,
        vib: 0.35,
        ..BASE
    },
    Look {
        id: "luminous",
        name: "Luminous",
        about: "Bright and airy, soft colours",
        curve: [&[(0.0, 0.04), (0.3, 0.37), (0.7, 0.8), (1.0, 0.985)], NONE, NONE, NONE],
        warmth: -0.03,
        sat: -0.12,
        vib: 0.05,
        bands: &[band(GREEN, 8.0, -0.1, 0.02), band(BLUE, -5.0, -0.05, 0.0)],
        shadows: tint(200.0, 0.01),
        ..BASE
    },
    Look {
        id: "golden",
        name: "Golden",
        about: "Warm late-afternoon light",
        curve: [&[(0.0, 0.0), (0.25, 0.235), (0.75, 0.78), (1.0, 1.0)], NONE, NONE, NONE],
        warmth: 0.25,
        vib: 0.1,
        bands: &[band(ORANGE, 0.0, 0.1, 0.0), band(BLUE, 0.0, -0.1, 0.0)],
        highlights: tint(AMBER, 0.025),
        ..BASE
    },
    Look {
        id: "crisp",
        name: "Crisp",
        about: "Cool, clean and clear",
        curve: [&[(0.0, 0.0), (0.25, 0.22), (0.75, 0.79), (1.0, 1.0)], NONE, NONE, NONE],
        warmth: -0.12,
        sat: -0.05,
        bands: &[band(BLUE, 0.0, 0.1, 0.0), band(AQUA, 0.0, 0.08, 0.0)],
        ..BASE
    },
    Look {
        id: "coast",
        name: "Coast",
        about: "Bright highlights, deep teal shadows",
        curve: [&[(0.0, 0.0), (0.2, 0.17), (0.5, 0.53), (0.8, 0.87), (1.0, 1.0)], NONE, NONE, NONE],
        sat: 0.1,
        vib: 0.2,
        shadows: tint(TEAL, 0.03),
        ..BASE
    },
    Look {
        id: "blossom",
        name: "Blossom",
        about: "Rich reds and yellows, cool whites",
        curve: [&[(0.0, 0.0), (0.25, 0.23), (0.75, 0.78), (1.0, 1.0)], NONE, NONE, NONE],
        warmth: 0.05,
        vib: 0.1,
        bands: &[band(RED, 0.0, 0.2, 0.0), band(ORANGE, 0.0, 0.12, 0.01), band(YELLOW, 0.0, 0.2, 0.0)],
        highlights: tint(230.0, 0.01),
        ..BASE
    },
    Look {
        id: "dusk",
        name: "Dusk",
        about: "Glowing orange and pink skies, for sunsets",
        curve: [&[(0.0, 0.0), (0.25, 0.21), (0.6, 0.62), (1.0, 1.0)], NONE, NONE, NONE],
        warmth: 0.12,
        tint: 0.015,
        sat: 0.05,
        vib: 0.15,
        bands: &[
            band(ORANGE, 0.0, 0.15, 0.0),
            band(RED, -4.0, 0.12, 0.0),
            band(YELLOW, -8.0, 0.1, 0.0),
            band(BLUE, 10.0, 0.05, 0.0),
            band(MAGENTA, 0.0, 0.15, 0.0),
        ],
        shadows: tint(PURPLE, 0.02),
        highlights: tint(ORANGE, 0.012),
        ..BASE
    },
    Look {
        id: "frost",
        name: "Frost",
        about: "Bright, clean whites and cool blues, for snow",
        curve: [&[(0.0, 0.01), (0.25, 0.29), (0.6, 0.7), (0.85, 0.94), (1.0, 1.0)], NONE, NONE, NONE],
        warmth: -0.15,
        sat: -0.1,
        vib: 0.1,
        bands: &[
            band(BLUE, -8.0, 0.2, 0.0),
            band(AQUA, 0.0, 0.1, 0.0),
            band(YELLOW, 0.0, -0.35, 0.02),
            band(ORANGE, 0.0, -0.05, 0.0),
        ],
        shadows: tint(235.0, 0.025),
        ..BASE
    },
    Look {
        id: "fresh",
        name: "Fresh",
        about: "Lively greens and bright petals, for flowers and gardens",
        curve: [&[(0.0, 0.0), (0.25, 0.23), (0.75, 0.79), (1.0, 1.0)], NONE, NONE, NONE],
        vib: 0.25,
        bands: &[
            band(GREEN, -6.0, 0.12, 0.02),
            band(MAGENTA, 0.0, 0.15, 0.0),
            band(RED, 0.0, 0.1, 0.0),
            band(PURPLE, 0.0, 0.12, 0.0),
            band(YELLOW, 0.0, 0.08, 0.0),
        ],
        ..BASE
    },
    // ---------- film ----------
    Look {
        id: "portrait160",
        name: "Portrait 160",
        group: Group::Film,
        about: "Natural colour and very soft contrast, like slow portrait negative film",
        curve: [&[(0.0, 0.025), (0.25, 0.255), (0.5, 0.51), (0.75, 0.765), (1.0, 0.975)], NONE, NONE, NONE],
        warmth: 0.03,
        sat: -0.12,
        bands: &[
            band(ORANGE, -2.0, -0.02, 0.015),
            band(RED, 0.0, -0.1, 0.0),
            band(GREEN, -6.0, -0.2, 0.0),
            band(BLUE, -6.0, -0.08, 0.0),
        ],
        shadows: tint(210.0, 0.006),
        grain: 0.08,
        ..BASE
    },
    Look {
        id: "portrait400",
        name: "Portrait 400",
        group: Group::Film,
        about: "Warm, soft skin and gentle contrast, like portrait colour negative film",
        curve: [&[(0.0, 0.03), (0.25, 0.245), (0.5, 0.52), (0.75, 0.78), (1.0, 0.97)], NONE, NONE, NONE],
        warmth: 0.08,
        sat: -0.08,
        vib: 0.05,
        bands: &[
            band(ORANGE, 0.0, 0.05, 0.02),
            band(RED, 5.0, -0.05, 0.0),
            band(GREEN, -10.0, -0.25, 0.0),
            band(BLUE, -8.0, -0.1, 0.0),
        ],
        shadows: tint(200.0, 0.008),
        highlights: tint(70.0, 0.015),
        grain: 0.15,
        ..BASE
    },
    Look {
        id: "pastel400",
        name: "Pastel 400",
        group: Group::Film,
        about: "Airy pastels with minty shadows, like overexposed 400H film",
        curve: [&[(0.0, 0.05), (0.3, 0.37), (0.7, 0.8), (1.0, 0.98)], NONE, NONE, NONE],
        warmth: -0.05,
        sat: -0.15,
        bands: &[band(GREEN, 15.0, -0.1, 0.02), band(BLUE, -10.0, -0.15, 0.0), band(RED, 0.0, -0.1, 0.0)],
        shadows: tint(165.0, 0.02),
        highlights: tint(340.0, 0.008),
        grain: 0.12,
        ..BASE
    },
    Look {
        id: "vivid100",
        name: "Vivid 100",
        group: Group::Film,
        about: "Saturated and punchy, like fine-grain landscape colour film",
        curve: [&[(0.0, 0.0), (0.25, 0.2), (0.5, 0.5), (0.75, 0.8), (1.0, 1.0)], NONE, NONE, NONE],
        warmth: 0.04,
        sat: 0.2,
        vib: 0.1,
        bands: &[band(RED, -3.0, 0.15, 0.0), band(BLUE, 0.0, 0.2, 0.0), band(ORANGE, -5.0, 0.0, -0.01)],
        grain: 0.05,
        ..BASE
    },
    Look {
        id: "slide50",
        name: "Slide 50",
        group: Group::Film,
        about: "Deep, saturated greens and blues, like slide film",
        curve: [&[(0.0, 0.0), (0.2, 0.14), (0.5, 0.5), (0.8, 0.87), (1.0, 1.0)], NONE, NONE, NONE],
        sat: 0.3,
        bands: &[
            band(GREEN, 5.0, 0.25, 0.0),
            band(BLUE, 5.0, 0.3, -0.01),
            band(RED, 0.0, 0.1, 0.0),
            band(YELLOW, 0.0, 0.15, 0.0),
        ],
        shadows: tint(320.0, 0.015),
        ..BASE
    },
    Look {
        id: "chrome",
        name: "Chrome",
        group: Group::Film,
        about: "Muted colour and firm shadows, a documentary look",
        curve: [&[(0.0, 0.02), (0.2, 0.15), (0.5, 0.49), (0.8, 0.8), (1.0, 0.95)], NONE, NONE, NONE],
        sat: -0.25,
        bands: &[
            band(BLUE, -15.0, -0.1, 0.0),
            band(RED, 5.0, -0.15, 0.0),
            band(GREEN, -8.0, -0.25, 0.0),
            band(YELLOW, 0.0, -0.2, 0.0),
            band(ORANGE, 0.0, -0.05, 0.0),
        ],
        shadows: tint(220.0, 0.01),
        highlights: tint(80.0, 0.008),
        ..BASE
    },
    Look {
        id: "classicneg",
        name: "Classic Neg",
        group: Group::Film,
        about: "Hard highlights, teal shadows, warm skin, like consumer negative film",
        curve: [&[(0.0, 0.04), (0.25, 0.22), (0.5, 0.5), (0.75, 0.82), (1.0, 0.97)], NONE, NONE, NONE],
        sat: -0.05,
        bands: &[
            band(GREEN, 20.0, -0.15, 0.0),
            band(RED, 6.0, 0.05, 0.0),
            band(YELLOW, -10.0, -0.1, 0.0),
            band(BLUE, 0.0, -0.05, 0.0),
        ],
        shadows: tint(200.0, 0.03),
        highlights: tint(50.0, 0.02),
        grain: 0.12,
        ..BASE
    },
    Look {
        id: "amber",
        name: "Amber",
        group: Group::Film,
        about: "Amber highlights and rich shadows, a nostalgic negative",
        curve: [&[(0.0, 0.03), (0.3, 0.3), (0.7, 0.74), (1.0, 0.97)], NONE, NONE, NONE],
        warmth: 0.1,
        sat: -0.05,
        bands: &[band(BLUE, 0.0, -0.25, 0.0), band(GREEN, -10.0, -0.2, 0.0), band(ORANGE, 0.0, 0.08, 0.0)],
        shadows: tint(30.0, 0.008),
        highlights: tint(AMBER, 0.035),
        ..BASE
    },
    Look {
        id: "night800",
        name: "Night 800",
        group: Group::Film,
        about: "Cool tungsten film with red glow around lights, made for night and neon",
        curve: [&[(0.0, 0.03), (0.25, 0.22), (0.75, 0.8), (1.0, 0.98)], NONE, NONE, NONE],
        warmth: -0.25,
        tint: -0.03,
        sat: -0.05,
        bands: &[band(RED, 0.0, 0.1, 0.0)],
        shadows: tint(TEAL, 0.03),
        highlights: tint(60.0, 0.01),
        glow: 0.55,
        grain: 0.3,
        ..BASE
    },
    Look {
        id: "gold200",
        name: "Gold 200",
        group: Group::Film,
        about: "Warm yellows and greenish shadows, like everyday drugstore film",
        curve: [&[(0.0, 0.02), (0.25, 0.24), (0.75, 0.79), (1.0, 0.98)], NONE, NONE, NONE],
        warmth: 0.18,
        sat: 0.08,
        bands: &[band(YELLOW, 0.0, 0.2, 0.0), band(RED, 0.0, 0.1, 0.0), band(BLUE, -8.0, -0.1, 0.0)],
        shadows: tint(150.0, 0.012),
        highlights: tint(85.0, 0.02),
        grain: 0.25,
        ..BASE
    },
    // ---------- cinematic ----------
    Look {
        id: "tealorange",
        name: "Teal & Orange",
        group: Group::Cinematic,
        about: "The blockbuster grade: teal shadows, warm skin",
        curve: [&[(0.0, 0.01), (0.25, 0.21), (0.75, 0.8), (1.0, 0.99)], NONE, NONE, NONE],
        bands: &[
            band(ORANGE, 0.0, 0.15, 0.0),
            band(GREEN, 40.0, -0.3, 0.0),
            band(BLUE, -15.0, 0.1, 0.0),
            band(YELLOW, -15.0, -0.1, 0.0),
        ],
        shadows: tint(TEAL, 0.045),
        highlights: tint(60.0, 0.03),
        ..BASE
    },
    Look {
        id: "neon",
        name: "Neon",
        group: Group::Cinematic,
        about: "Magenta and cyan city lights, deep blacks, for night",
        curve: [&[(0.0, 0.0), (0.2, 0.14), (0.5, 0.5), (0.85, 0.9), (1.0, 1.0)], NONE, NONE, NONE],
        sat: 0.1,
        vib: 0.2,
        bands: &[
            band(MAGENTA, 0.0, 0.25, 0.0),
            band(PURPLE, 10.0, 0.2, 0.0),
            band(BLUE, 15.0, 0.15, 0.0),
            band(AQUA, 0.0, 0.25, 0.0),
            band(ORANGE, -5.0, -0.15, 0.0),
            band(YELLOW, -10.0, -0.3, 0.0),
            band(GREEN, 40.0, -0.2, 0.0),
        ],
        shadows: tint(270.0, 0.03),
        highlights: tint(MAGENTA, 0.008),
        glow: 0.2,
        grain: 0.15,
        ..BASE
    },
    Look {
        id: "moody",
        name: "Moody",
        group: Group::Cinematic,
        about: "Dark and muted, with olive greens",
        curve: [&[(0.0, 0.05), (0.25, 0.19), (0.6, 0.55), (1.0, 0.92)], NONE, NONE, NONE],
        sat: -0.3,
        bands: &[band(GREEN, -20.0, -0.3, -0.03), band(BLUE, -10.0, -0.2, 0.0), band(ORANGE, 0.0, 0.05, 0.0)],
        shadows: tint(220.0, 0.02),
        vignette: -0.35,
        ..BASE
    },
    Look {
        id: "matte",
        name: "Matte",
        group: Group::Cinematic,
        about: "Faded blacks and soft contrast",
        curve: [&[(0.0, 0.1), (0.25, 0.28), (0.75, 0.76), (1.0, 0.94)], NONE, NONE, NONE],
        warmth: 0.04,
        sat: -0.1,
        highlights: tint(70.0, 0.01),
        ..BASE
    },
    Look {
        id: "bleach",
        name: "Bleach Bypass",
        group: Group::Cinematic,
        about: "Silvery, low colour and hard contrast",
        curve: [&[(0.0, 0.0), (0.2, 0.12), (0.5, 0.5), (0.8, 0.9), (1.0, 1.0)], NONE, NONE, NONE],
        sat: -0.55,
        vib: -0.2,
        grain: 0.1,
        ..BASE
    },
    // ---------- vintage ----------
    Look {
        id: "seventies",
        name: "Seventies",
        group: Group::Vintage,
        about: "Faded, warm and yellow, like an old print",
        curve: [
            &[(0.0, 0.07), (0.3, 0.32), (0.7, 0.75), (1.0, 0.93)],
            &[(0.0, 0.02), (0.5, 0.54), (1.0, 1.0)],
            NONE,
            &[(0.0, 0.12), (1.0, 0.85)],
        ],
        warmth: 0.15,
        sat: -0.1,
        bands: &[band(GREEN, -15.0, -0.2, 0.0)],
        ..BASE
    },
    Look {
        id: "instant",
        name: "Instant",
        group: Group::Vintage,
        about: "Cyan shadows, warm highlights and soft corners, like an instant print",
        curve: [&[(0.0, 0.08), (0.25, 0.3), (0.75, 0.8), (1.0, 0.95)], NONE, NONE, NONE],
        sat: -0.1,
        shadows: tint(180.0, 0.03),
        highlights: tint(70.0, 0.03),
        vignette: -0.3,
        grain: 0.1,
        ..BASE
    },
    // ---------- black and white ----------
    Look {
        id: "mono",
        name: "Mono",
        group: Group::Mono,
        about: "Clean black and white",
        curve: [&[(0.0, 0.0), (0.25, 0.23), (0.75, 0.78), (1.0, 1.0)], NONE, NONE, NONE],
        mono: Some([0.25, 0.65, 0.1]),
        ..BASE
    },
    Look {
        id: "noir",
        name: "Noir",
        group: Group::Mono,
        about: "High-contrast black and white with dark skies",
        curve: [&[(0.0, 0.0), (0.2, 0.1), (0.5, 0.5), (0.8, 0.9), (1.0, 1.0)], NONE, NONE, NONE],
        mono: Some([0.55, 0.4, 0.05]),
        vignette: -0.4,
        ..BASE
    },
    Look {
        id: "grain400",
        name: "Grain 400",
        group: Group::Mono,
        about: "Punchy, grainy black and white, like classic 400-speed film",
        curve: [&[(0.0, 0.02), (0.2, 0.15), (0.5, 0.5), (0.8, 0.86), (1.0, 0.99)], NONE, NONE, NONE],
        mono: Some([0.3, 0.6, 0.1]),
        grain: 0.45,
        ..BASE
    },
    Look {
        id: "silver",
        name: "Silver",
        group: Group::Mono,
        about: "Soft, cool black and white",
        curve: [&[(0.0, 0.08), (0.3, 0.33), (0.7, 0.72), (1.0, 0.95)], NONE, NONE, NONE],
        mono: Some([0.25, 0.6, 0.15]),
        shadows: tint(250.0, 0.006),
        highlights: tint(250.0, 0.008),
        ..BASE
    },
    Look {
        id: "sepia",
        name: "Sepia",
        group: Group::Mono,
        about: "Warm brown tones, like an old photograph",
        curve: [&[(0.0, 0.04), (0.5, 0.52), (1.0, 0.96)], NONE, NONE, NONE],
        mono: Some([0.3, 0.6, 0.1]),
        shadows: tint(50.0, 0.03),
        highlights: tint(AMBER, 0.035),
        ..BASE
    },
];

pub fn find(id: &str) -> Option<&'static Look> {
    LOOKS.iter().find(|l| l.id == id)
}

/// What a scene looks like, as recognised from its content (see `photos_ml`), for
/// suggesting filters and tuning Enhance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scene {
    Portrait,
    Landscape,
    Food,
    Night,
    City,
    /// Pages, notes, receipts, screenshots.
    Document,
    /// Sunrise and sunset skies: their warmth is meant, not a cast.
    Sunset,
    /// Animals, pets mostly.
    Pet,
    /// Flowers and gardens close up.
    Flowers,
    /// Snow, which should stay bright and white.
    Snow,
    Other,
}

impl Scene {
    pub const ALL: [Scene; 11] = [
        Scene::Portrait,
        Scene::Landscape,
        Scene::Food,
        Scene::Night,
        Scene::City,
        Scene::Document,
        Scene::Sunset,
        Scene::Pet,
        Scene::Flowers,
        Scene::Snow,
        Scene::Other,
    ];
}

/// Filters that tend to suit a scene, best first.
pub fn suggested(scene: Scene) -> &'static [&'static str] {
    match scene {
        Scene::Portrait => &["portrait400", "portrait160", "pastel400", "amber", "mono"],
        Scene::Landscape => &["slide50", "vivid", "vivid100", "chrome"],
        Scene::Food => &["golden", "vivid100", "amber"],
        Scene::Night => &["night800", "neon", "tealorange", "noir"],
        Scene::City => &["chrome", "classicneg", "tealorange", "grain400"],
        Scene::Document => &["crisp", "mono", "silver"],
        Scene::Sunset => &["dusk", "golden", "amber", "tealorange"],
        Scene::Pet => &["portrait160", "golden", "vivid", "grain400"],
        Scene::Flowers => &["fresh", "blossom", "pastel400", "vivid100"],
        Scene::Snow => &["frost", "crisp", "luminous", "silver"],
        Scene::Other => &["vivid", "crisp", "chrome"],
    }
}

/// A filter as the editor lists it.
#[derive(Debug, Clone, Serialize)]
pub struct Listed {
    pub id: &'static str,
    pub name: &'static str,
    pub group: Group,
    pub about: &'static str,
}

pub fn catalog() -> Vec<Listed> {
    LOOKS.iter().map(|l| Listed { id: l.id, name: l.name, group: l.group, about: l.about }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_suggestions_exist() {
        let mut ids: Vec<_> = LOOKS.iter().map(|l| l.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), LOOKS.len());
        for scene in Scene::ALL {
            for id in suggested(scene) {
                assert!(find(id).is_some(), "{id}");
            }
        }
    }
}
