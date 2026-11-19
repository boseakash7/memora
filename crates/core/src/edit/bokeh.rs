//! Background blur that looks like a lens did it, behind the subject the mask finds.
//!
//! A camera doesn't smudge the background like a Gaussian: every point spreads into the shape
//! of the aperture (a disc), bright points become visible bokeh balls, and the blur grows
//! with distance, so what is right behind the subject is only a little soft. With no depth
//! map, distance from the subject stands in for depth. Styles:
//!
//! - `Lens`: round bokeh, highlights bloom into discs, gentler next to the subject.
//! - `Oval`: the same with tall oval bokeh, like an anamorphic cinema lens.
//! - `Soft`: a smooth Gaussian, the plainest.
//! - `Swirl`: blur running around the centre, stronger towards the edges, like old
//!   Petzval and Helios lenses.
//! - `Motion`: sideways streaks, as when panning with a moving subject.
//! - `Zoom`: streaks out from the subject, as when zooming during the exposure.
//!
//! All work at a reduced size (the effect is several pixels wide at any size) on colours
//! weighted by how much background each pixel is, so the subject never bleeds into the blur.

use image::{GrayImage, RgbImage};
use rayon::prelude::*;

use super::color::{mix, smoothstep};
use super::render::{Blur, Geometry, Map, fit, gaussian, sample, sample_mask};
use super::{BlurStyle, Edit};

/// Colour times background weight (r, g, b) and the weight.
type Px = [f32; 4];

pub(super) fn background_blur(src: &RgbImage, mask: &GrayImage, edit: &Edit, fw: u32, fh: u32) -> Blur {
    let style = edit.blur_style.resolved();
    let long_out = fw.max(fh) as f32;
    // The lens disc's radius: up to 3 % of the long side.
    let r_out = (edit.blur / 100.0 * 0.03 * long_out).max(0.5);
    // Work at a size where that radius is a handful of pixels.
    let target = match style {
        BlurStyle::Soft => 6.0,
        BlurStyle::Lens | BlurStyle::Oval => 7.0,
        _ => 8.0,
    };
    let long = ((long_out * target / r_out).round() as u32).clamp(48, 720);
    let (bw, bh) = fit(fw, fh, long, long);
    let r = r_out * bw.max(bh) as f32 / long_out;
    let geom = Geometry::new(edit, src.width(), src.height());
    // Samples per work pixel each way: enough that a big photo doesn't alias.
    let density = (src.width().max(src.height()) as f32 / long as f32).max(1.0);
    let ss = (density.ceil() as usize).clamp(1, 3);
    let bokeh = matches!(style, BlurStyle::Lens | BlurStyle::Oval | BlurStyle::Swirl);

    let (w, h) = (bw as usize, bh as usize);
    let sampled: Vec<(Px, f32)> = (0..w * h)
        .into_par_iter()
        .map(|k| {
            let (i, j) = (k % w, k / w);
            let mut c = [0.0f32; 3];
            for sy in 0..ss {
                for sx in 0..ss {
                    let u = (i as f32 + (sx as f32 + 0.5) / ss as f32) / bw as f32;
                    let v = (j as f32 + (sy as f32 + 0.5) / ss as f32) / bh as f32;
                    let (x, y) = geom.source_point(u, v);
                    let p = sample(src, x, y);
                    c = [c[0] + p[0], c[1] + p[1], c[2] + p[2]];
                }
            }
            let n = (ss * ss) as f32;
            let mut c = [c[0] / n, c[1] / n, c[2] / n];
            let (sx, sy) = geom.source_point((i as f32 + 0.5) / bw as f32, (j as f32 + 0.5) / bh as f32);
            let m = sample_mask(mask, sx / src.width() as f32, sy / src.height() as f32);
            if bokeh {
                // Highlights were clipped by the camera; give them back some of their energy
                // so they spread into bright discs, as they do through a real lens.
                let hi = c[0].max(c[1]).max(c[2]);
                let k = 1.0 + 1.6 * smoothstep(0.75, 1.0, hi);
                c = [c[0] * k, c[1] * k, c[2] * k];
            }
            let wt = (1.0 - m).powi(2);
            ([c[0] * wt, c[1] * wt, c[2] * wt, wt], m)
        })
        .collect();
    let px: Vec<Px> = sampled.iter().map(|s| s.0).collect();
    let subject: Vec<f32> = sampled.iter().map(|s| s.1).collect();

    let out = match style {
        BlurStyle::Soft => {
            let mut planes = split(&px);
            for p in &mut planes {
                gaussian(p, w, h, r * 0.55);
            }
            join(&planes)
        }
        BlurStyle::Lens | BlurStyle::Oval => {
            let rx = if style == BlurStyle::Oval { r * 0.62 } else { r };
            let far = disc(&px, w, h, rx, r);
            let close = disc(&px, w, h, rx * 0.4, r * 0.4);
            // Close to the subject (as a stand-in for depth), less blur.
            let mut near = subject.clone();
            gaussian(&mut near, w, h, 0.05 * w.max(h) as f32);
            far.into_par_iter()
                .zip(close)
                .zip(near)
                .map(|((f, c), n)| {
                    let t = smoothstep(0.04, 0.45, n) * 0.85;
                    [mix(f[0], c[0], t), mix(f[1], c[1], t), mix(f[2], c[2], t), mix(f[3], c[3], t)]
                })
                .collect()
        }
        BlurStyle::Swirl => {
            let swirled = swirl(&px, w, h, r);
            let mut planes = split(&swirled);
            for p in &mut planes {
                gaussian(p, w, h, r * 0.25);
            }
            join(&planes)
        }
        BlurStyle::Motion => motion(&px, w, h, r * 2.2),
        BlurStyle::Zoom => {
            let centre = centroid(&subject, w, h);
            zoom(&px, w, h, centre, edit.blur / 100.0 * 0.22)
        }
        BlurStyle::Unknown => unreachable!("resolved above"),
    };
    let planes = split(&out);
    let [r, g, b, wt] = planes;
    let map = |data: Vec<f32>| Map { w: bw, h: bh, data };
    Blur { rgb: [map(r), map(g), map(b)], weight: map(wt) }
}

fn split(px: &[Px]) -> [Vec<f32>; 4] {
    std::array::from_fn(|c| px.iter().map(|p| p[c]).collect())
}

fn join(planes: &[Vec<f32>; 4]) -> Vec<Px> {
    (0..planes[0].len()).map(|k| [planes[0][k], planes[1][k], planes[2][k], planes[3][k]]).collect()
}

#[inline]
fn at(px: &[Px], w: usize, h: usize, x: isize, y: isize) -> Px {
    px[y.clamp(0, h as isize - 1) as usize * w + x.clamp(0, w as isize - 1) as usize]
}

/// Bilinear sample of the weighted colours.
#[inline]
fn sample_px(px: &[Px], w: usize, h: usize, x: f32, y: f32) -> Px {
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let (x0, y0) = (x0 as isize, y0 as isize);
    let (a, b, c, d) = (at(px, w, h, x0, y0), at(px, w, h, x0 + 1, y0), at(px, w, h, x0, y0 + 1), at(px, w, h, x0 + 1, y0 + 1));
    std::array::from_fn(|k| mix(mix(a[k], b[k], tx), mix(c[k], d[k], tx), ty))
}

/// Flat disc (or oval) kernel: every point spreads evenly over the aperture's shape. Its
/// edge is softened by one pixel, so discs don't look cut out.
fn disc(px: &[Px], w: usize, h: usize, rx: f32, ry: f32) -> Vec<Px> {
    let (rx, ry) = (rx.max(0.5), ry.max(0.5));
    let (ix, iy) = (rx.ceil() as isize + 1, ry.ceil() as isize + 1);
    let mut taps: Vec<(isize, isize, f32)> = Vec::new();
    for dy in -iy..=iy {
        for dx in -ix..=ix {
            let d = ((dx as f32 / rx).powi(2) + (dy as f32 / ry).powi(2)).sqrt();
            // Distance past the edge in pixels, roughly.
            let past = (d - 1.0) * rx.min(ry);
            let wt = (0.5 - past).clamp(0.0, 1.0);
            if wt > 0.0 {
                taps.push((dx, dy, wt));
            }
        }
    }
    let total: f32 = taps.iter().map(|t| t.2).sum();
    (0..w * h)
        .into_par_iter()
        .map(|k| {
            let (x, y) = ((k % w) as isize, (k / w) as isize);
            let mut acc = [0.0f32; 4];
            for &(dx, dy, wt) in &taps {
                let p = at(px, w, h, x + dx, y + dy);
                for c in 0..4 {
                    acc[c] += p[c] * wt;
                }
            }
            acc.map(|v| v / total)
        })
        .collect()
}

/// Blur along circles around the centre, growing towards the edges.
fn swirl(px: &[Px], w: usize, h: usize, r: f32) -> Vec<Px> {
    const N: usize = 15;
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let rmax = (cx * cx + cy * cy).sqrt();
    (0..w * h)
        .into_par_iter()
        .map(|k| {
            let (x, y) = ((k % w) as f32 + 0.5 - cx, (k / w) as f32 + 0.5 - cy);
            let rr = (x * x + y * y).sqrt().max(1.0);
            // Arc length in pixels: small in the middle, about 3 radii at the corners.
            let len = r * 3.0 * (rr / rmax).powf(1.5);
            let span = len / rr;
            let a0 = y.atan2(x);
            let mut acc = [0.0f32; 4];
            for s in 0..N {
                let a = a0 + span * (s as f32 / (N - 1) as f32 - 0.5);
                let p = sample_px(px, w, h, cx + rr * a.cos(), cy + rr * a.sin());
                for c in 0..4 {
                    acc[c] += p[c];
                }
            }
            acc.map(|v| v / N as f32)
        })
        .collect()
}

/// Sideways streaks: two box blurs along each row (a smooth, even streak).
fn motion(px: &[Px], w: usize, h: usize, half: f32) -> Vec<Px> {
    let r = (half / 2.0).round().max(1.0) as isize;
    let pass = |src: &[Px]| -> Vec<Px> {
        let rows: Vec<Vec<Px>> = (0..h)
            .into_par_iter()
            .map(|y| {
                let row = &src[y * w..(y + 1) * w];
                let get = |x: isize| row[x.clamp(0, w as isize - 1) as usize];
                let n = (2 * r + 1) as f32;
                let mut acc = [0.0f32; 4];
                for x in -r..=r {
                    let p = get(x);
                    for c in 0..4 {
                        acc[c] += p[c];
                    }
                }
                let mut out = Vec::with_capacity(w);
                for x in 0..w as isize {
                    out.push(acc.map(|v| v / n));
                    let (add, sub) = (get(x + r + 1), get(x - r));
                    for c in 0..4 {
                        acc[c] += add[c] - sub[c];
                    }
                }
                out
            })
            .collect();
        rows.concat()
    };
    pass(&pass(px))
}

/// Where the subject is (centre of the mask), in pixels; the frame's centre without one.
fn centroid(subject: &[f32], w: usize, h: usize) -> (f32, f32) {
    let (mut sx, mut sy, mut s) = (0.0f64, 0.0f64, 0.0f64);
    for (k, m) in subject.iter().enumerate() {
        let m = *m as f64;
        sx += m * ((k % w) as f64 + 0.5);
        sy += m * ((k / w) as f64 + 0.5);
        s += m;
    }
    if s < 1.0 { (w as f32 / 2.0, h as f32 / 2.0) } else { ((sx / s) as f32, (sy / s) as f32) }
}

/// Streaks pointing at `centre`, longer further away. `amount` is the streak length as a
/// share of the distance.
fn zoom(px: &[Px], w: usize, h: usize, centre: (f32, f32), amount: f32) -> Vec<Px> {
    const N: usize = 18;
    (0..w * h)
        .into_par_iter()
        .map(|k| {
            let (x, y) = ((k % w) as f32 + 0.5, (k / w) as f32 + 0.5);
            let (dx, dy) = (x - centre.0, y - centre.1);
            let mut acc = [0.0f32; 4];
            for s in 0..N {
                let t = 1.0 - amount * s as f32 / (N - 1) as f32;
                let p = sample_px(px, w, h, centre.0 + dx * t, centre.1 + dy * t);
                for c in 0..4 {
                    acc[c] += p[c];
                }
            }
            acc.map(|v| v / N as f32)
        })
        .collect()
}
