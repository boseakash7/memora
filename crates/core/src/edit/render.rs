//! Applying an [`Edit`] to a picture.
//!
//! One pass per output pixel, in parallel by rows: sample the source through the geometry
//! (bilinear, in linear light), background blur, white balance and exposure, dehaze, tone
//! (blacks, whites, shadows, highlights, contrast, clarity), saturation and vibrance, the
//! filter's look (mixed by its strength), vignette and grain. Sharpening is a second pass.
//!
//! Effects that look at the neighbourhood (shadows/highlights, clarity, dehaze, glow,
//! background blur) use small blurred maps of the whole frame, sized relative to the picture,
//! so a 400 px thumbnail and a 6000 px export of the same edit look the same.

use image::{GrayImage, RgbImage};
use rayon::prelude::*;

use super::color::{self, Curve, Rgb, mix, mix3, smoothstep};
use super::filters::{self, Look};
use super::{Adjust, Edit};

/// Long side of the low-resolution maps.
const MAP_LONG: u32 = 256;

/// Render `edit` of `src` (as displayed, EXIF rotation applied, at any resolution) to fit in
/// `max_w` x `max_h`. Never larger than the edit gives at the source's resolution.
/// `mask` (the main subject, white; any resolution, same framing as `src`) is used for the
/// background blur; without one there is no blur.
pub fn render(src: &RgbImage, mask: Option<&GrayImage>, edit: &Edit, max_w: u32, max_h: u32) -> RgbImage {
    let (ow, oh) = edit.output_size(src.width(), src.height());
    let (fw, fh) = fit(ow, oh, max_w, max_h);

    // Sample from a copy at most ~1.5x the needed density: bilinear sampling of a much bigger
    // picture would alias.
    let density = ow as f32 / fw as f32;
    let scaled;
    let src = if density > 2.0 {
        let k = density / 1.5;
        let (w, h) = (((src.width() as f32 / k).round() as u32).max(1), ((src.height() as f32 / k).round() as u32).max(1));
        scaled = image::imageops::thumbnail(src, w, h);
        &scaled
    } else {
        src
    };

    let geom = Geometry::new(edit, src.width(), src.height());
    let mut out = RgbImage::new(fw, fh);
    if fw == 0 || fh == 0 {
        return out;
    }
    let p = Pipeline::new(edit, src, mask, fw, fh);
    let row_len = fw as usize * 3;
    out.par_chunks_mut(row_len).enumerate().for_each(|(j, row)| {
        let v = (j as f32 + 0.5) / fh as f32;
        for i in 0..fw as usize {
            let u = (i as f32 + 0.5) / fw as f32;
            let (sx, sy) = geom.source_point(u, v);
            let c = sample(src, sx, sy);
            let m = p.mask.as_ref().map(|m| sample_mask(m, sx / src.width() as f32, sy / src.height() as f32));
            let g = p.pixel(c, m, u, v, i as u32, j as u32);
            row[i * 3] = to_u8(g[0]);
            row[i * 3 + 1] = to_u8(g[1]);
            row[i * 3 + 2] = to_u8(g[2]);
        }
    });
    if edit.adjust.sharpen > 0.0 {
        sharpen(&mut out, edit.adjust.sharpen / 100.0);
    }
    out
}

/// Render the frame at map size without colour changes (for statistics: Enhance).
pub fn sample_frame(src: &RgbImage, edit: &Edit, long: u32) -> (Vec<Rgb>, u32, u32) {
    let (ow, oh) = edit.output_size(src.width(), src.height());
    let (w, h) = fit(ow, oh, long, long);
    let geom = Geometry::new(edit, src.width(), src.height());
    (sample_grid_avg(src, &geom, w, h), w, h)
}

pub(super) fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let s = (max_w as f32 / w.max(1) as f32).min(max_h as f32 / h.max(1) as f32).min(1.0);
    (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1))
}

#[inline]
fn exp2(x: f32) -> f32 {
    x.exp2()
}

#[inline]
fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

// ---------- geometry ----------

/// Maps output positions to source pixels: crop, straighten, quarter turns, flip.
pub(super) struct Geometry {
    sw: f32,
    sh: f32,
    bw: f32,
    bh: f32,
    rotate: u8,
    flip: bool,
    cos: f32,
    sin: f32,
    x0: f32,
    y0: f32,
    cw: f32,
    ch: f32,
}

impl Geometry {
    pub(super) fn new(edit: &Edit, sw: u32, sh: u32) -> Geometry {
        let (sw, sh) = (sw as f32, sh as f32);
        let (bw, bh) = if edit.rotate % 2 == 1 { (sh, sw) } else { (sw, sh) };
        let c = edit.crop.unwrap_or(super::Crop { x: 0.0, y: 0.0, w: 1.0, h: 1.0 });
        let a = edit.straighten.to_radians();
        Geometry {
            sw,
            sh,
            bw,
            bh,
            rotate: edit.rotate % 4,
            flip: edit.flip,
            cos: a.cos(),
            sin: a.sin(),
            x0: c.x * bw,
            y0: c.y * bh,
            cw: c.w * bw,
            ch: c.h * bh,
        }
    }

    /// Source pixel coordinates (continuous, pixel centres at +0.5) for output position
    /// `u`, `v` (0..1 across the output).
    #[inline]
    pub(super) fn source_point(&self, u: f32, v: f32) -> (f32, f32) {
        let (px, py) = (self.x0 + u * self.cw, self.y0 + v * self.ch);
        // The picture is turned clockwise by the straighten angle around its centre, so the
        // point shown at p comes from p turned back.
        let (dx, dy) = (px - self.bw / 2.0, py - self.bh / 2.0);
        let qx = self.cos * dx + self.sin * dy + self.bw / 2.0;
        let qy = -self.sin * dx + self.cos * dy + self.bh / 2.0;
        let (x, y) = match self.rotate {
            1 => (qy, self.sh - qx),
            2 => (self.sw - qx, self.sh - qy),
            3 => (self.sw - qy, qx),
            _ => (qx, qy),
        };
        (if self.flip { self.sw - x } else { x }, y)
    }
}

/// Bilinear sample in linear light; outside the picture the edge pixels repeat.
#[inline]
pub(super) fn sample(src: &RgbImage, x: f32, y: f32) -> Rgb {
    let (w, h) = (src.width() as i32, src.height() as i32);
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let (x0, y0) = (x0 as i32, y0 as i32);
    let raw = src.as_raw();
    let dec = color::decode_u8();
    let at = |x: i32, y: i32| -> Rgb {
        let (x, y) = (x.clamp(0, w - 1) as usize, y.clamp(0, h - 1) as usize);
        let k = (y * w as usize + x) * 3;
        [dec[raw[k] as usize], dec[raw[k + 1] as usize], dec[raw[k + 2] as usize]]
    };
    let top = mix3(at(x0, y0), at(x0 + 1, y0), tx);
    let bottom = mix3(at(x0, y0 + 1), at(x0 + 1, y0 + 1), tx);
    mix3(top, bottom, ty)
}

/// Mask value (0..1) at fractions `u`, `v` of the source.
#[inline]
pub(super) fn sample_mask(m: &GrayImage, u: f32, v: f32) -> f32 {
    let (w, h) = (m.width() as i32, m.height() as i32);
    let (fx, fy) = (u * w as f32 - 0.5, v * h as f32 - 0.5);
    let (x0, y0) = (fx.floor(), fy.floor());
    let (tx, ty) = (fx - x0, fy - y0);
    let (x0, y0) = (x0 as i32, y0 as i32);
    let raw = m.as_raw();
    let at = |x: i32, y: i32| raw[(y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize] as f32 / 255.0;
    mix(mix(at(x0, y0), at(x0 + 1, y0), tx), mix(at(x0, y0 + 1), at(x0 + 1, y0 + 1), tx), ty)
}

/// The frame sampled on a `w` x `h` grid, each cell the average of 4x4 samples, so a small map of a big picture
/// doesn't alias. Cheaper than shrinking the picture first.
fn sample_grid_avg(src: &RgbImage, geom: &Geometry, w: u32, h: u32) -> Vec<Rgb> {
    const N: u32 = 4;
    (0..w * h)
        .into_par_iter()
        .map(|k| {
            let (i, j) = (k % w, k / w);
            let mut acc = [0.0f32; 3];
            for sy in 0..N {
                for sx in 0..N {
                    let u = (i as f32 + (sx as f32 + 0.5) / N as f32) / w as f32;
                    let v = (j as f32 + (sy as f32 + 0.5) / N as f32) / h as f32;
                    let (x, y) = geom.source_point(u, v);
                    let c = sample(src, x, y);
                    acc = [acc[0] + c[0], acc[1] + c[1], acc[2] + c[2]];
                }
            }
            let n = (N * N) as f32;
            [acc[0] / n, acc[1] / n, acc[2] / n]
        })
        .collect()
}

// ---------- small maps ----------

/// A single-channel map covering the output frame.
pub(super) struct Map {
    pub(super) w: u32,
    pub(super) h: u32,
    pub(super) data: Vec<f32>,
}

impl Map {
    #[inline]
    fn at(&self, u: f32, v: f32) -> f32 {
        let (w, h) = (self.w as i32, self.h as i32);
        let (fx, fy) = (u * w as f32 - 0.5, v * h as f32 - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let d = &self.data;
        let at = |x: i32, y: i32| d[(y.clamp(0, h - 1) * w + x.clamp(0, w - 1)) as usize];
        mix(mix(at(x0, y0), at(x0 + 1, y0), tx), mix(at(x0, y0 + 1), at(x0 + 1, y0 + 1), tx), ty)
    }

    fn blurred(&self, sigma: f32) -> Map {
        let mut data = self.data.clone();
        gaussian(&mut data, self.w as usize, self.h as usize, sigma);
        Map { w: self.w, h: self.h, data }
    }
}

/// Approximate Gaussian blur: three box blurs each way.
pub(super) fn gaussian(data: &mut [f32], w: usize, h: usize, sigma: f32) {
    if sigma < 0.3 || w == 0 || h == 0 {
        return;
    }
    // Box radius for three passes (Kovesi).
    let wide = ((12.0 * sigma * sigma / 3.0 + 1.0).sqrt()).floor() as usize;
    let r = (wide.max(1) - 1) / 2;
    let r = r.max(1);
    let mut tmp = vec![0.0f32; data.len()];
    for _ in 0..3 {
        box_rows(data, &mut tmp, w, h, r);
        box_cols(&tmp, data, w, h, r);
    }
}

fn box_rows(src: &[f32], dst: &mut [f32], w: usize, _h: usize, r: usize) {
    dst.par_chunks_mut(w).zip(src.par_chunks(w)).for_each(|(d, s)| {
        let n = (2 * r + 1) as f32;
        let at = |i: isize| s[i.clamp(0, w as isize - 1) as usize];
        let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
        for (x, out) in d.iter_mut().enumerate() {
            *out = acc / n;
            acc += at(x as isize + r as isize + 1) - at(x as isize - r as isize);
        }
    });
}

fn box_cols(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize) {
    let n = (2 * r + 1) as f32;
    let cols: Vec<Vec<f32>> = (0..w)
        .into_par_iter()
        .map(|x| {
            let at = |y: isize| src[y.clamp(0, h as isize - 1) as usize * w + x];
            let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
            let mut col = Vec::with_capacity(h);
            for y in 0..h {
                col.push(acc / n);
                acc += at(y as isize + r as isize + 1) - at(y as isize - r as isize);
            }
            col
        })
        .collect();
    for (x, col) in cols.into_iter().enumerate() {
        for (y, v) in col.into_iter().enumerate() {
            dst[y * w + x] = v;
        }
    }
}

/// Minimum over a square window (for the dark channel).
fn min_filter(data: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    let mut rows = vec![0.0f32; data.len()];
    for y in 0..h {
        for x in 0..w {
            let (a, b) = (x.saturating_sub(r), (x + r).min(w - 1));
            rows[y * w + x] = data[y * w + a..=y * w + b].iter().copied().fold(f32::MAX, f32::min);
        }
    }
    let mut out = vec![0.0f32; data.len()];
    for y in 0..h {
        let (a, b) = (y.saturating_sub(r), (y + r).min(h - 1));
        for x in 0..w {
            out[y * w + x] = (a..=b).map(|yy| rows[yy * w + x]).fold(f32::MAX, f32::min);
        }
    }
    out
}

// ---------- pipeline ----------

struct Dehaze {
    amount: f32,
    air: Rgb,
    dark: Map,
}

pub(super) struct Blur {
    /// Background colours blurred, weighted by how much background each pixel is.
    pub(super) rgb: [Map; 3],
    pub(super) weight: Map,
}

struct LookStage {
    look: &'static Look,
    strength: f32,
    curves: [Option<Curve>; 4],
    wb: Rgb,
}

struct Pipeline {
    wb: Rgb,
    gain: f32,
    a: Adjust,
    tone: bool,
    /// Perceptual luminance blurred wide (shadows/highlights) and narrow (clarity).
    wide: Option<Map>,
    narrow: Option<Map>,
    glow: Option<(Map, f32)>,
    dehaze: Option<Dehaze>,
    blur: Option<Blur>,
    mask: Option<GrayImage>,
    look: Option<LookStage>,
    vignette: f32,
    grain: f32,
    /// Grain cells per output pixel across, and the amplitude scale that follows from it.
    grain_cells: f32,
    grain_amp: f32,
    aspect: f32,
    /// Something can push colours past white (so highlights get a soft shoulder).
    brightens: bool,
}

/// White balance multipliers for Temperature/Tint values in -1..1, keeping white's
/// luminance.
pub(crate) fn white_balance(temperature: f32, tint: f32) -> Rgb {
    let m = [exp2(0.4 * temperature), exp2(-0.25 * tint), exp2(-0.4 * temperature)];
    let y = color::luma(m);
    [m[0] / y, m[1] / y, m[2] / y]
}

impl Pipeline {
    fn new(edit: &Edit, src: &RgbImage, mask: Option<&GrayImage>, fw: u32, fh: u32) -> Pipeline {
        let a = edit.adjust;
        let look = edit.filter.as_deref().and_then(filters::find).filter(|_| edit.filter_strength > 0.0).map(|look| {
            let c = |pts: &'static [(f32, f32)]| (!pts.is_empty()).then(|| Curve::new(pts));
            LookStage {
                look,
                strength: edit.filter_strength,
                curves: [c(look.curve[0]), c(look.curve[1]), c(look.curve[2]), c(look.curve[3])],
                wb: white_balance(look.warmth, look.tint),
            }
        });
        let wb = white_balance(a.temperature / 100.0, a.tint / 100.0);
        let gain = exp2(a.exposure / 50.0);
        let tone = a.highlights != 0.0 || a.shadows != 0.0 || a.clarity != 0.0;
        let glow_amount = look.as_ref().map(|l| l.look.glow * l.strength).unwrap_or(0.0);
        let blur_on = edit.blur > 0.0 && mask.is_some();

        let needs_maps = tone || a.dehaze != 0.0 || glow_amount > 0.0 || blur_on;
        let (mut wide, mut narrow, mut glow, mut dehaze, mut blur) = (None, None, None, None, None);
        if needs_maps {
            let (mw, mh) = fit(fw, fh, MAP_LONG, MAP_LONG);
            let geom = Geometry::new(edit, src.width(), src.height());
            let pixels = sample_grid_avg(src, &geom, mw, mh);
            let scale = mw.max(mh) as f32 / MAP_LONG as f32;
            let lum: Vec<f32> =
                pixels.iter().map(|c| color::perceive((color::luma(*c) * gain).max(0.0))).collect();
            let base = Map { w: mw, h: mh, data: lum };
            if tone {
                wide = Some(base.blurred(9.0 * scale));
                narrow = Some(base.blurred(2.5 * scale));
            }
            if glow_amount > 0.0 {
                let bright: Vec<f32> = pixels.iter().map(|c| ((color::luma(*c) * gain) - 0.55).max(0.0)).collect();
                glow = Some((Map { w: mw, h: mh, data: bright }.blurred(5.0 * scale), glow_amount));
            }
            if a.dehaze != 0.0 {
                let darkest: Vec<f32> =
                    pixels.iter().map(|c| (c[0] * wb[0]).min(c[1] * wb[1]).min(c[2] * wb[2]) * gain).collect();
                let dark = min_filter(&darkest, mw as usize, mh as usize, (3.0 * scale).round().max(1.0) as usize);
                // Airlight: the average colour of the haziest 0.5 %.
                let mut order: Vec<usize> = (0..dark.len()).collect();
                order.sort_by(|&x, &y| dark[y].total_cmp(&dark[x]));
                let top = &order[..(dark.len() / 200).max(1)];
                let mut air = [0.0f32; 3];
                for &k in top {
                    for ch in 0..3 {
                        air[ch] += pixels[k][ch] * wb[ch] * gain;
                    }
                }
                let air = air.map(|v| (v / top.len() as f32).clamp(0.3, 1.0));
                dehaze = Some(Dehaze {
                    amount: a.dehaze / 100.0,
                    air,
                    dark: Map { w: mw, h: mh, data: dark }.blurred(3.0 * scale),
                });
            }
            if blur_on {
                blur = Some(super::bokeh::background_blur(src, mask.unwrap(), edit, fw, fh));
            }
        }

        let vignette = a.vignette / 100.0 + look.as_ref().map(|l| l.look.vignette * l.strength).unwrap_or(0.0);
        let grain = a.grain / 100.0 + look.as_ref().map(|l| l.look.grain * l.strength).unwrap_or(0.0);
        // Grain cells: 1/1400 of the long side, whatever the resolution. Below one cell per
        // pixel the grain averages out, as it would when the picture is shrunk.
        let long = fw.max(fh) as f32;
        let cells_per_px = 1400.0 / long;
        Pipeline {
            wb,
            gain,
            a,
            tone,
            wide,
            narrow,
            glow,
            dehaze,
            blur,
            mask: mask.filter(|_| blur_on).cloned(),
            look,
            vignette,
            grain,
            grain_cells: cells_per_px,
            grain_amp: (1.0 / cells_per_px).min(1.0),
            aspect: fw as f32 / fh as f32,
            brightens: gain > 1.0
                || a.whites > 0.0
                || a.highlights > 0.0
                || a.shadows > 0.0
                || a.dehaze > 0.0
                || a.clarity > 0.0
                || a.contrast > 0.0
                || blur_on
                || wb.iter().any(|m| *m > 1.0001),
        }
    }

    /// One pixel: linear `c` in, sRGB-encoded out.
    #[inline]
    fn pixel(&self, c: Rgb, mask: Option<f32>, u: f32, v: f32, i: u32, j: u32) -> Rgb {
        let a = &self.a;
        let mut c = c;

        if let (Some(b), Some(m)) = (&self.blur, mask) {
            let w = b.weight.at(u, v);
            if w > 1e-4 {
                let bg = [b.rgb[0].at(u, v) / w, b.rgb[1].at(u, v) / w, b.rgb[2].at(u, v) / w];
                // Fully blurred where the mask says background.
                let keep = smoothstep(0.15, 0.85, m);
                c = mix3(bg, c, keep);
            }
        }

        // White balance and exposure.
        c = [c[0] * self.wb[0] * self.gain, c[1] * self.wb[1] * self.gain, c[2] * self.wb[2] * self.gain];

        if let Some(d) = &self.dehaze {
            let dark = d.dark.at(u, v);
            if d.amount > 0.0 {
                let t = (1.0 - 0.95 * d.amount * dark / d.air.iter().copied().fold(f32::MAX, f32::min)).max(0.25);
                for (v, air) in c.iter_mut().zip(d.air) {
                    *v = ((*v - air) / t + air).max(0.0);
                }
            } else {
                let k = -d.amount * 0.5;
                for (v, air) in c.iter_mut().zip(d.air) {
                    *v = mix(*v, air, k);
                }
            }
        }

        // Tone, on perceptual luminance; colours are scaled to follow it.
        let y = color::luma(c).max(0.0);
        let mut l = color::perceive(y);
        let l0 = l;
        if a.blacks != 0.0 {
            l += a.blacks / 100.0 * 0.15 * (1.0 - l).max(0.0).powi(4);
        }
        if a.whites != 0.0 {
            l += a.whites / 100.0 * 0.2 * l.clamp(0.0, 1.0).powi(4);
        }
        if self.tone {
            let wide = self.wide.as_ref().map(|m| m.at(u, v)).unwrap_or(l);
            let local = mix(l, wide, 0.7);
            if a.shadows != 0.0 {
                let w = 1.0 - smoothstep(0.0, 0.6, local);
                l *= exp2(a.shadows / 100.0 * 1.4 * w);
            }
            if a.highlights != 0.0 {
                let w = smoothstep(0.4, 1.0, local);
                if a.highlights < 0.0 {
                    l *= exp2(a.highlights / 100.0 * 1.1 * w);
                } else {
                    l += a.highlights / 100.0 * 0.35 * w * (1.0 - l).max(0.0);
                }
            }
            if a.clarity != 0.0 {
                let narrow = self.narrow.as_ref().map(|m| m.at(u, v)).unwrap_or(l);
                let mid = 4.0 * l.clamp(0.0, 1.0) * (1.0 - l.clamp(0.0, 1.0));
                l += a.clarity / 100.0 * 1.3 * (l - narrow) * (0.3 + 0.7 * mid);
            }
        }
        if a.contrast != 0.0 {
            let k = a.contrast / 100.0 * 0.9;
            let x = l.clamp(0.0, 1.0);
            l += k * x * (1.0 - x) * (2.0 * x - 1.0) * 2.0;
        }
        if l != l0 {
            let l = l.max(0.0);
            let y2 = color::unperceive(l);
            if y > 1e-5 {
                let r = y2 / y;
                c = [c[0] * r, c[1] * r, c[2] * r];
            } else {
                c = [y2, y2, y2];
            }
        }

        // Saturation and vibrance in Oklab.
        if a.saturation != 0.0 || a.vibrance != 0.0 {
            let lab = color::to_oklab(c);
            let chroma = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
            let mut k = 1.0 + a.saturation / 100.0;
            if a.vibrance != 0.0 {
                let muted = 1.0 - smoothstep(0.0, 0.2, chroma);
                let h = color::hue_deg(lab[1], lab[2]);
                let skin = (-(color::hue_dist(h, 58.0) / 25.0).powi(2)).exp();
                let vib = a.vibrance / 100.0;
                let protect = if vib > 0.0 { 1.0 - 0.6 * skin } else { 1.0 };
                k *= 1.0 + vib * 0.9 * muted * protect;
            }
            c = color::from_oklab([lab[0], lab[1] * k.max(0.0), lab[2] * k.max(0.0)]);
        }

        // Highlights pushed past white roll off instead of clipping hard; without brightening,
        // colours are left exactly as they are.
        c = if self.brightens { shoulder(c) } else { clip(c) };
        c = color::into_gamut(c);
        let mut g = color::encode3(c);

        if let Some(ls) = &self.look {
            let glow = self.glow.as_ref().map(|(m, amt)| m.at(u, v) * amt).unwrap_or(0.0);
            let looked = apply_look(ls, g, glow);
            g = mix3(g, looked, ls.strength);
        }

        if self.vignette != 0.0 {
            let dx = (u - 0.5) * 2.0;
            let dy = (v - 0.5) * 2.0;
            // Rounder than the frame: a wide photo's corners darken, its sides less so.
            let (dx, dy) = if self.aspect > 1.0 { (dx, dy / self.aspect.sqrt()) } else { (dx * self.aspect.sqrt(), dy) };
            let r = (dx * dx + dy * dy).sqrt();
            let w = smoothstep(0.45, 1.35, r);
            if self.vignette < 0.0 {
                let f = 1.0 + self.vignette * 0.85 * w;
                g = [g[0] * f, g[1] * f, g[2] * f];
            } else {
                let f = self.vignette * 0.8 * w;
                g = [mix(g[0], 1.0, f), mix(g[1], 1.0, f), mix(g[2], 1.0, f)];
            }
        }

        if self.grain > 0.0 {
            let lum = (g[0] * 0.2126 + g[1] * 0.7152 + g[2] * 0.0722).clamp(0.0, 1.0);
            let n = value_noise(i as f32 * self.grain_cells, j as f32 * self.grain_cells);
            let amp = self.grain * 0.13 * self.grain_amp * (0.25 + 3.0 * lum * (1.0 - lum));
            g = [g[0] + n * amp, g[1] + n * amp, g[2] + n * amp];
        }
        g
    }
}

/// Soft shoulder above 0.8 (linear), per channel.
#[inline]
fn shoulder(c: Rgb) -> Rgb {
    let s = |x: f32| if x <= 0.8 { x } else { 0.8 + 0.2 * (1.0 - (-(x - 0.8) / 0.2).exp()) };
    let hi = c[0].max(c[1]).max(c[2]);
    if hi <= 0.8 {
        return c;
    }
    // Scale all channels by the brightest one's curve, which keeps the hue.
    let k = s(hi) / hi;
    [c[0] * k, c[1] * k, c[2] * k]
}

/// Scale a colour brighter than white down to white, keeping its hue.
#[inline]
fn clip(c: Rgb) -> Rgb {
    let hi = c[0].max(c[1]).max(c[2]);
    if hi <= 1.0 { c } else { [c[0] / hi, c[1] / hi, c[2] / hi] }
}

/// The filter's look on an sRGB-encoded colour.
fn apply_look(ls: &LookStage, g: Rgb, glow: f32) -> Rgb {
    let look = ls.look;
    let mut g = g;
    if let Some(m) = &ls.curves[0] {
        g = [m.apply(g[0]), m.apply(g[1]), m.apply(g[2])];
    }
    for (v, curve) in g.iter_mut().zip(&ls.curves[1..]) {
        if let Some(cv) = curve {
            *v = cv.apply(*v);
        }
    }
    let mut c = color::decode3(g);
    if look.warmth != 0.0 || look.tint != 0.0 {
        c = [c[0] * ls.wb[0], c[1] * ls.wb[1], c[2] * ls.wb[2]];
    }
    if glow > 0.0 {
        // Halation: light scattered back through the film's red-sensitive layer.
        c = [c[0] + glow * 0.9, c[1] + glow * 0.28, c[2] + glow * 0.12];
    }
    if let Some(w) = look.mono {
        let y = c[0] * w[0] + c[1] * w[1] + c[2] * w[2];
        c = [y, y, y];
    }
    let mut lab = color::to_oklab(c);
    let chroma = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
    if chroma > 1e-4 && (!look.bands.is_empty() || look.sat != 0.0 || look.vib != 0.0) {
        let mut h = color::hue_deg(lab[1], lab[2]);
        let mut k = 1.0 + look.sat;
        if look.vib != 0.0 {
            k *= 1.0 + look.vib * (1.0 - smoothstep(0.0, 0.2, chroma));
        }
        let mut dl = 0.0;
        // Bands only touch coloured pixels, so greys stay grey.
        let colourful = smoothstep(0.01, 0.05, chroma);
        for b in look.bands {
            let d = color::hue_dist(h, b.hue);
            if d.abs() >= 45.0 {
                continue;
            }
            let w = 0.5 * (1.0 + (d / 45.0 * std::f32::consts::PI).cos()) * colourful;
            h += b.shift * w;
            k *= 1.0 + b.sat * w;
            dl += b.light * w * (chroma / 0.15).min(1.0);
        }
        let nc = chroma * k.max(0.0);
        let hr = h.to_radians();
        lab = [lab[0] + dl, nc * hr.cos(), nc * hr.sin()];
    }
    let (sh, hi) = (look.shadows, look.highlights);
    if sh.amount != 0.0 || hi.amount != 0.0 {
        let l = lab[0].clamp(0.0, 1.0);
        let ws = (1.0 - l).powi(2);
        let wh = l * l;
        let (sr, hr) = (sh.hue.to_radians(), hi.hue.to_radians());
        lab[1] += sh.amount * ws * sr.cos() + hi.amount * wh * hr.cos();
        lab[2] += sh.amount * ws * sr.sin() + hi.amount * wh * hr.sin();
    }
    let c = color::into_gamut(clip(color::from_oklab(lab)));
    color::encode3(c)
}

/// Smooth noise in -1..1 (roughly unit variance after scaling) on a grid of cells.
#[inline]
fn value_noise(x: f32, y: f32) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (tx, ty) = (x - x0, y - y0);
    let (xi, yi) = (x0 as i32, y0 as i32);
    let h = |x: i32, y: i32| -> f32 {
        let mut n = (x as u32).wrapping_mul(0x27d4_eb2d) ^ (y as u32).wrapping_mul(0x1656_67b1);
        n ^= n >> 15;
        n = n.wrapping_mul(0x2c1b_3c6d);
        n ^= n >> 12;
        n = n.wrapping_mul(0x297a_2d39);
        n ^= n >> 15;
        (n as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    let sx = tx * tx * (3.0 - 2.0 * tx);
    let sy = ty * ty * (3.0 - 2.0 * ty);
    let v = mix(mix(h(xi, yi), h(xi + 1, yi), sx), mix(h(xi, yi + 1), h(xi + 1, yi + 1), sx), sy);
    v * 1.7
}

/// Unsharp mask on luminance (no colour fringes), 3x3.
fn sharpen(img: &mut RgbImage, amount: f32) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w < 3 || h < 3 {
        return;
    }
    let lum: Vec<f32> = img.pixels().map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32).collect();
    let k = amount * 1.6;
    img.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        for x in 0..w {
            let mut sum = 0.0;
            for dy in -1i32..=1 {
                let yy = (y as i32 + dy).clamp(0, h as i32 - 1) as usize;
                for dx in -1i32..=1 {
                    let xx = (x as i32 + dx).clamp(0, w as i32 - 1) as usize;
                    sum += lum[yy * w + xx];
                }
            }
            let d = lum[y * w + x] - sum / 9.0;
            // Ignore the smallest differences (noise).
            let d = if d.abs() < 2.0 { 0.0 } else { d - 2.0 * d.signum() };
            for ch in 0..3 {
                let v = row[x * 3 + ch] as f32 + k * d;
                row[x * 3 + ch] = v.clamp(0.0, 255.0).round() as u8;
            }
        }
    });
}
