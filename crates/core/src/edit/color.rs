//! Colour math for the editor: sRGB transfer, Oklab, tone curves.
//!
//! Pixels are processed as linear-light `f32` RGB. Oklab (Björn Ottosson, 2020) is used for
//! saturation, hue and toning because changing chroma there keeps hue and lightness steady,
//! which plain HSL does not.

use std::sync::OnceLock;

pub type Rgb = [f32; 3];

/// Rec. 709 / sRGB luminance weights (linear light).
pub const LUMA: Rgb = [0.2126, 0.7152, 0.0722];

#[inline]
pub fn luma(c: Rgb) -> f32 {
    c[0] * LUMA[0] + c[1] * LUMA[1] + c[2] * LUMA[2]
}

#[inline]
pub fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
pub fn mix3(a: Rgb, b: Rgb, t: f32) -> Rgb {
    [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
}

#[inline]
pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn srgb_to_linear_exact(v: f32) -> f32 {
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb_exact(v: f32) -> f32 {
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// 8-bit sRGB to linear.
pub fn decode_u8() -> &'static [f32; 256] {
    static T: OnceLock<[f32; 256]> = OnceLock::new();
    T.get_or_init(|| std::array::from_fn(|i| srgb_to_linear_exact(i as f32 / 255.0)))
}

const ENC_N: usize = 4096;

fn encode_table() -> &'static [f32] {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    // Indexed by the square root of the linear value, which spends the entries on the
    // shadows, where sRGB changes fastest.
    T.get_or_init(|| (0..=ENC_N).map(|i| linear_to_srgb_exact((i as f32 / ENC_N as f32).powi(2))).collect())
}

fn decode_table() -> &'static [f32] {
    static T: OnceLock<Vec<f32>> = OnceLock::new();
    T.get_or_init(|| (0..=ENC_N).map(|i| srgb_to_linear_exact(i as f32 / ENC_N as f32)).collect())
}

#[inline]
fn lerp_table(t: &[f32], x: f32) -> f32 {
    let f = x * (t.len() - 1) as f32;
    let i = (f as usize).min(t.len() - 2);
    let frac = f - i as f32;
    t[i] + (t[i + 1] - t[i]) * frac
}

/// Linear to sRGB-encoded, for values in [0, 1] (others are clamped).
#[inline]
pub fn encode(v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    if v >= 1.0 {
        return 1.0;
    }
    lerp_table(encode_table(), v.sqrt())
}

/// sRGB-encoded to linear, for values in [0, 1] (others are clamped).
#[inline]
pub fn decode(v: f32) -> f32 {
    lerp_table(decode_table(), v.clamp(0.0, 1.0))
}

#[inline]
pub fn encode3(c: Rgb) -> Rgb {
    [encode(c[0]), encode(c[1]), encode(c[2])]
}

#[inline]
pub fn decode3(c: Rgb) -> Rgb {
    [decode(c[0]), decode(c[1]), decode(c[2])]
}

/// Perceptual lightness of a linear luminance, roughly what the eye sees as even steps.
/// The inverse of [`unperceive`]. Cheap gamma 2.2 through the sRGB tables.
#[inline]
pub fn perceive(y: f32) -> f32 {
    if y <= 1.0 { encode(y) } else { y.powf(1.0 / 2.4) }
}

#[inline]
pub fn unperceive(l: f32) -> f32 {
    if l <= 1.0 { decode(l) } else { ((l + 0.055) / 1.055).powf(2.4) }
}

/// Fast cube root for Oklab (inputs are small and non-negative after clamping).
#[inline]
fn cbrt(x: f32) -> f32 {
    x.cbrt()
}

/// Linear sRGB to Oklab `[L, a, b]`.
#[inline]
pub fn to_oklab(c: Rgb) -> Rgb {
    let [r, g, b] = [c[0].max(0.0), c[1].max(0.0), c[2].max(0.0)];
    let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l, m, s) = (cbrt(l), cbrt(m), cbrt(s));
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// Oklab to linear sRGB (may be out of gamut; callers clamp).
#[inline]
pub fn from_oklab(c: Rgb) -> Rgb {
    let l = c[0] + 0.396_337_78 * c[1] + 0.215_803_76 * c[2];
    let m = c[0] - 0.105_561_346 * c[1] - 0.063_854_17 * c[2];
    let s = c[0] - 0.089_484_18 * c[1] - 1.291_485_5 * c[2];
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// Bring an out-of-gamut linear colour back by moving it towards grey of the same
/// luminance (keeps hue better than clamping each channel).
#[inline]
pub fn into_gamut(c: Rgb) -> Rgb {
    let y = luma(c).clamp(0.0, 1.0);
    let lo = c[0].min(c[1]).min(c[2]);
    let hi = c[0].max(c[1]).max(c[2]);
    let mut t: f32 = 1.0;
    if lo < 0.0 && y - lo > 1e-6 {
        t = t.min(y / (y - lo));
    }
    if hi > 1.0 && hi - y > 1e-6 {
        t = t.min((1.0 - y) / (hi - y));
    }
    if t >= 1.0 {
        return c;
    }
    let t = t.max(0.0);
    [y + (c[0] - y) * t, y + (c[1] - y) * t, y + (c[2] - y) * t]
}

/// Hue angle of an Oklab colour in degrees, 0..360.
#[inline]
pub fn hue_deg(a: f32, b: f32) -> f32 {
    let h = b.atan2(a).to_degrees();
    if h < 0.0 { h + 360.0 } else { h }
}

/// Shortest signed distance between two hue angles in degrees.
#[inline]
pub fn hue_dist(a: f32, b: f32) -> f32 {
    let mut d = (a - b) % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d < -180.0 {
        d += 360.0;
    }
    d
}

/// A tone curve through control points (x and y in 0..1), evaluated by monotone cubic
/// (Fritsch–Carlson) interpolation so it never overshoots, then tabulated.
#[derive(Debug, Clone)]
pub struct Curve {
    table: Vec<f32>,
}

const CURVE_N: usize = 1024;

impl Curve {
    pub fn new(points: &[(f32, f32)]) -> Curve {
        let mut pts: Vec<(f32, f32)> = points.to_vec();
        pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-6);
        if pts.is_empty() {
            pts = vec![(0.0, 0.0), (1.0, 1.0)];
        }
        if pts.len() == 1 {
            let (_, y) = pts[0];
            return Curve { table: vec![y; CURVE_N + 1] };
        }
        let n = pts.len();
        let d: Vec<f32> = (0..n - 1).map(|i| (pts[i + 1].1 - pts[i].1) / (pts[i + 1].0 - pts[i].0)).collect();
        let mut m = vec![0.0f32; n];
        m[0] = d[0];
        m[n - 1] = d[n - 2];
        for i in 1..n - 1 {
            m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
        }
        for i in 0..n - 1 {
            if d[i] == 0.0 {
                m[i] = 0.0;
                m[i + 1] = 0.0;
                continue;
            }
            let (a, b) = (m[i] / d[i], m[i + 1] / d[i]);
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[i] = t * a * d[i];
                m[i + 1] = t * b * d[i];
            }
        }
        let eval = |x: f32| -> f32 {
            if x <= pts[0].0 {
                return pts[0].1;
            }
            if x >= pts[n - 1].0 {
                return pts[n - 1].1;
            }
            let i = pts.windows(2).position(|w| x <= w[1].0).unwrap_or(n - 2);
            let (x0, y0) = pts[i];
            let (x1, y1) = pts[i + 1];
            let h = x1 - x0;
            let t = (x - x0) / h;
            let (t2, t3) = (t * t, t * t * t);
            (2.0 * t3 - 3.0 * t2 + 1.0) * y0
                + (t3 - 2.0 * t2 + t) * h * m[i]
                + (-2.0 * t3 + 3.0 * t2) * y1
                + (t3 - t2) * h * m[i + 1]
        };
        Curve { table: (0..=CURVE_N).map(|i| eval(i as f32 / CURVE_N as f32)).collect() }
    }

    #[inline]
    pub fn apply(&self, x: f32) -> f32 {
        lerp_table(&self.table, x.clamp(0.0, 1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_round_trip() {
        for i in 0..=255 {
            let v = i as f32 / 255.0;
            let back = encode(decode(v));
            assert!((back - v).abs() < 1.5e-3, "{v} -> {back}");
            assert!((decode(v) - srgb_to_linear_exact(v)).abs() < 1e-3);
        }
        assert!((encode(0.18) - linear_to_srgb_exact(0.18)).abs() < 1e-4);
    }

    #[test]
    fn oklab_round_trip_and_white() {
        let w = to_oklab([1.0, 1.0, 1.0]);
        assert!((w[0] - 1.0).abs() < 1e-3 && w[1].abs() < 1e-3 && w[2].abs() < 1e-3);
        for c in [[0.2, 0.5, 0.1], [0.9, 0.1, 0.4], [0.01, 0.02, 0.5]] {
            let back = from_oklab(to_oklab(c));
            for k in 0..3 {
                assert!((back[k] - c[k]).abs() < 1e-3, "{c:?} -> {back:?}");
            }
        }
        // sRGB red's Oklab hue is about 29 degrees.
        let r = to_oklab([1.0, 0.0, 0.0]);
        assert!((hue_deg(r[1], r[2]) - 29.2).abs() < 1.0);
    }

    #[test]
    fn curves_pass_through_points_and_stay_monotone() {
        let c = Curve::new(&[(0.0, 0.05), (0.25, 0.2), (0.75, 0.82), (1.0, 0.97)]);
        assert!((c.apply(0.0) - 0.05).abs() < 1e-3);
        assert!((c.apply(0.25) - 0.2).abs() < 1e-3);
        assert!((c.apply(1.0) - 0.97).abs() < 1e-3);
        let mut last = -1.0;
        for i in 0..=100 {
            let y = c.apply(i as f32 / 100.0);
            assert!(y >= last - 1e-6);
            last = y;
        }
        assert!((Curve::new(&[]).apply(0.4) - 0.4).abs() < 1e-3);
    }

    #[test]
    fn gamut_mapping_keeps_luminance() {
        let c = into_gamut([1.4, 0.2, -0.1]);
        assert!(c.iter().all(|v| (-1e-4..=1.0001).contains(v)), "{c:?}");
        assert!((luma(c) - luma([1.4, 0.2, -0.1]).clamp(0.0, 1.0)).abs() < 1e-3);
    }
}
