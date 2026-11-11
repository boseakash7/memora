//! Faces: YuNet finds them (with five landmarks each), SFace turns each one into a
//! 128-number vector; vectors of the same person point the same way.
//!
//! Both models are from the OpenCV Zoo (Apache-2.0). Pre- and post-processing follow
//! OpenCV's `FaceDetectorYN` and `FaceRecognizerSF`.

use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;

use crate::{Error, Result};

/// YuNet's fixed input size.
const INPUT: u32 = 640;
const STRIDES: [usize; 3] = [8, 16, 32];
/// Detections below this confidence are dropped. Real faces score about 0.9; animal faces
/// and patterns that look a bit like faces score up to about 0.8.
pub const MIN_SCORE: f32 = 0.85;
/// Faces smaller than this (pixels in the analysed picture) are too blurry to recognise.
pub const MIN_SIZE: f32 = 32.0;
const NMS_IOU: f32 = 0.3;

#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    /// x, y, width, height in pixels.
    pub rect: [f32; 4],
    pub score: f32,
    /// Eyes (left and right in the picture), nose tip, mouth corners.
    pub landmarks: [[f32; 2]; 5],
}

/// A picture ready for the detector: the whole picture and, if it is big, four overlapping
/// quarters, each scaled to fit 640x640. Made on the decoding threads, so the models
/// never wait for resizing.
pub struct Prepared {
    size: (u32, u32),
    regions: Vec<Region>,
}

struct Region {
    /// x, y, width, height in the picture.
    area: [u32; 4],
    scale: f32,
    pixels: RgbImage,
}

pub fn prepare(img: &RgbImage) -> Result<Prepared> {
    let (w, h) = img.dimensions();
    let mut areas = vec![[0, 0, w, h]];
    if w.max(h) > 960 {
        let (tw, th) = ((w as f32 * 0.6).ceil() as u32, (h as f32 * 0.6).ceil() as u32);
        areas.extend([(0, 0), (w - tw, 0), (0, h - th), (w - tw, h - th)].map(|(x, y)| [x, y, tw, th]));
    }
    let mut resizer = Resizer::new();
    let regions = areas
        .into_iter()
        .map(|area @ [rx, ry, rw, rh]| {
            let scale = INPUT as f32 / rw.max(rh) as f32;
            let sw = ((rw as f32 * scale).round() as u32).clamp(1, INPUT);
            let sh = ((rh as f32 * scale).round() as u32).clamp(1, INPUT);
            let mut pixels = RgbImage::new(sw, sh);
            let options = ResizeOptions::new()
                .crop(rx as f64, ry as f64, rw as f64, rh as f64)
                .resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
            resizer.resize(img, &mut pixels, &options).map_err(|e| Error::Model(format!("cannot resize: {e}")))?;
            Ok(Region { area, scale, pixels })
        })
        .collect::<Result<_>>()?;
    Ok(Prepared { size: (w, h), regions })
}

pub struct Detector {
    session: Session,
}

impl Detector {
    pub fn new(session: Session) -> Detector {
        Detector { session }
    }

    /// Faces in a prepared picture, best first.
    pub fn detect(&mut self, prepared: &Prepared) -> Result<Vec<Detection>> {
        let mut found = Vec::new();
        for region in &prepared.regions {
            found.extend(self.detect_region(prepared.size, region)?);
        }
        let mut faces = nms(found);
        faces.retain(|d| d.rect[2].min(d.rect[3]) >= MIN_SIZE);
        Ok(faces)
    }

    fn detect_region(&mut self, (iw, ih): (u32, u32), region: &Region) -> Result<Vec<Detection>> {
        let [rx, ry, rw, rh] = region.area;
        let scale = region.scale;
        // BGR planes, 0-255, the rest of the square left black.
        let plane = (INPUT * INPUT) as usize;
        let mut input = vec![0f32; 3 * plane];
        for (x, y, p) in region.pixels.enumerate_pixels() {
            let i = (y * INPUT + x) as usize;
            input[i] = p[2] as f32;
            input[plane + i] = p[1] as f32;
            input[2 * plane + i] = p[0] as f32;
        }
        let tensor = Tensor::from_array(([1usize, 3, INPUT as usize, INPUT as usize], input))?;
        let outputs = self.session.run(ort::inputs!["input" => tensor])?;

        // Faces cut by an edge of a quarter (not of the picture) are found whole elsewhere.
        let margin = 2.0;
        let inner_left = rx > 0;
        let inner_top = ry > 0;
        let inner_right = rx + rw < iw;
        let inner_bottom = ry + rh < ih;

        let mut found = Vec::new();
        for stride in STRIDES {
            let get = |name: &str| -> Result<&[f32]> {
                let key = format!("{name}_{stride}");
                let value = outputs.get(key.as_str()).ok_or_else(|| Error::Model(format!("YuNet has no output {key}")))?;
                Ok(value.try_extract_tensor::<f32>()?.1)
            };
            let (cls, obj, bbox, kps) = (get("cls")?, get("obj")?, get("bbox")?, get("kps")?);
            let cols = INPUT as usize / stride;
            let s = stride as f32;
            for idx in 0..cols * cols {
                let score = (cls[idx].clamp(0.0, 1.0) * obj[idx].clamp(0.0, 1.0)).sqrt();
                if score < MIN_SCORE {
                    continue;
                }
                let (c, r) = ((idx % cols) as f32, (idx / cols) as f32);
                let b = &bbox[idx * 4..idx * 4 + 4];
                let (cx, cy) = ((c + b[0]) * s, (r + b[1]) * s);
                let (bw, bh) = (b[2].exp() * s, b[3].exp() * s);
                // Back to picture pixels.
                let to_img = |x: f32, y: f32| [x / scale + rx as f32, y / scale + ry as f32];
                let [x1, y1] = to_img(cx - bw / 2.0, cy - bh / 2.0);
                let (w, h) = (bw / scale, bh / scale);
                if (inner_left && x1 < rx as f32 + margin)
                    || (inner_top && y1 < ry as f32 + margin)
                    || (inner_right && x1 + w > (rx + rw) as f32 - margin)
                    || (inner_bottom && y1 + h > (ry + rh) as f32 - margin)
                {
                    continue;
                }
                let k = &kps[idx * 10..idx * 10 + 10];
                let mut landmarks = [[0f32; 2]; 5];
                for (n, l) in landmarks.iter_mut().enumerate() {
                    *l = to_img((k[2 * n] + c) * s, (k[2 * n + 1] + r) * s);
                }
                // Clip the box to the picture.
                let (x0, y0) = (x1.max(0.0), y1.max(0.0));
                let rect = [x0, y0, (x1 + w).min(iw as f32) - x0, (y1 + h).min(ih as f32) - y0];
                if rect[2] > 0.0 && rect[3] > 0.0 {
                    found.push(Detection { rect, score, landmarks });
                }
            }
        }
        Ok(found)
    }
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let w = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
    let h = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
    if w <= 0.0 || h <= 0.0 {
        return 0.0;
    }
    let inter = w * h;
    inter / (a[2] * a[3] + b[2] * b[3] - inter)
}

/// Keep the best of overlapping detections.
fn nms(mut found: Vec<Detection>) -> Vec<Detection> {
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Detection> = Vec::new();
    for d in found {
        if kept.iter().all(|k| iou(&k.rect, &d.rect) < NMS_IOU) {
            kept.push(d);
        }
    }
    kept
}

/// Where the five landmarks go in SFace's 112x112 input (the usual ArcFace template).
const TEMPLATE: [[f32; 2]; 5] =
    [[38.2946, 51.6963], [73.5318, 51.5014], [56.0252, 71.7366], [41.5493, 92.3655], [70.7299, 92.2041]];
const FACE_INPUT: u32 = 112;

pub struct Embedder {
    session: Session,
}

impl Embedder {
    pub fn new(session: Session) -> Embedder {
        Embedder { session }
    }

    /// Unit-length vector for a detected face.
    pub fn embed(&mut self, img: &RgbImage, face: &Detection) -> Result<Vec<f32>> {
        let aligned = align(img, &face.landmarks);
        // RGB planes, 0-255.
        let plane = (FACE_INPUT * FACE_INPUT) as usize;
        let mut input = vec![0f32; 3 * plane];
        for (x, y, p) in aligned.enumerate_pixels() {
            let i = (y * FACE_INPUT + x) as usize;
            for c in 0..3 {
                input[c * plane + i] = p[c] as f32;
            }
        }
        let tensor = Tensor::from_array(([1usize, 3, FACE_INPUT as usize, FACE_INPUT as usize], input))?;
        let outputs = self.session.run(ort::inputs!["data" => tensor])?;
        let mut v = outputs[0].try_extract_tensor::<f32>()?.1.to_vec();
        photos_core::people::normalize(&mut v);
        Ok(v)
    }
}

/// Rotate, scale and move the face so its landmarks sit on the template (least-squares
/// similarity transform), sampled bilinearly into a 112x112 picture.
pub fn align(img: &RgbImage, landmarks: &[[f32; 2]; 5]) -> RgbImage {
    let mean = |pts: &[[f32; 2]; 5]| {
        let (sx, sy) = pts.iter().fold((0.0, 0.0), |(x, y), p| (x + p[0], y + p[1]));
        (sx / 5.0, sy / 5.0)
    };
    let (mx, my) = mean(landmarks);
    let (ux, uy) = mean(&TEMPLATE);
    let (mut na, mut nb, mut den) = (0f32, 0f32, 0f32);
    for (s, d) in landmarks.iter().zip(&TEMPLATE) {
        let (x, y, u, v) = (s[0] - mx, s[1] - my, d[0] - ux, d[1] - uy);
        na += x * u + y * v;
        nb += x * v - y * u;
        den += x * x + y * y;
    }
    let den = den.max(1e-6);
    let (a, b) = (na / den, nb / den);
    // template = [a -b; b a] * picture + t
    let (tx, ty) = (ux - (a * mx - b * my), uy - (b * mx + a * my));
    let k = (a * a + b * b).max(1e-12);

    let (w, h) = (img.width() as i64, img.height() as i64);
    let pixel = |x: i64, y: i64| -> [f32; 3] {
        if x < 0 || y < 0 || x >= w || y >= h {
            return [0.0; 3];
        }
        let p = img.get_pixel(x as u32, y as u32).0;
        [p[0] as f32, p[1] as f32, p[2] as f32]
    };
    RgbImage::from_fn(FACE_INPUT, FACE_INPUT, |u, v| {
        let (du, dv) = (u as f32 - tx, v as f32 - ty);
        let sx = (a * du + b * dv) / k;
        let sy = (-b * du + a * dv) / k;
        let (x0, y0) = (sx.floor() as i64, sy.floor() as i64);
        let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
        let (p00, p10, p01, p11) = (pixel(x0, y0), pixel(x0 + 1, y0), pixel(x0, y0 + 1), pixel(x0 + 1, y0 + 1));
        let mut out = [0u8; 3];
        for c in 0..3 {
            let top = p00[c] * (1.0 - fx) + p10[c] * fx;
            let bottom = p01[c] * (1.0 - fx) + p11[c] * fx;
            out[c] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
        }
        image::Rgb(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alignment_maps_landmarks_onto_the_template() {
        // A picture where each pixel encodes its own position, and landmarks that are the
        // template rotated by 30 degrees, scaled by 2 and moved.
        let img = RgbImage::from_fn(400, 400, |x, y| image::Rgb([(x / 2) as u8, (y / 2) as u8, 0]));
        let (c, s) = (30f32.to_radians().cos(), 30f32.to_radians().sin());
        let lm = TEMPLATE.map(|[x, y]| [2.0 * (c * x - s * y) + 150.0, 2.0 * (s * x + c * y) + 80.0]);
        let out = align(&img, &lm);
        for (t, l) in TEMPLATE.iter().zip(&lm) {
            let p = out.get_pixel(t[0].round() as u32, t[1].round() as u32).0;
            // Each template point shows the picture pixel at its landmark (within rounding).
            assert!((p[0] as f32 - l[0] / 2.0).abs() <= 1.5, "{p:?} vs {l:?}");
            assert!((p[1] as f32 - l[1] / 2.0).abs() <= 1.5, "{p:?} vs {l:?}");
        }
    }

    #[test]
    fn nms_keeps_the_best_of_overlaps() {
        let d = |x: f32, score| Detection { rect: [x, 0.0, 10.0, 10.0], score, landmarks: [[0.0; 2]; 5] };
        let kept = nms(vec![d(0.0, 0.8), d(1.0, 0.9), d(50.0, 0.85)]);
        assert_eq!(kept.iter().map(|k| k.score).collect::<Vec<_>>(), [0.9, 0.85]);
    }
}
