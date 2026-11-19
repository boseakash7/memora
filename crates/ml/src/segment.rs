//! The main subject of a photo (people, a pet, an object in front), for the editor's
//! background blur. BiRefNet-lite (MIT): a square picture in, a matte (logits) out.
//! Preprocessing follows its Hugging Face pipeline: stretched to the square, ImageNet mean/std.
//!
//! Two exports of the same model: 1024x1024 (onnx-community) for graphics cards, and
//! 512x512 (studioludens) for PCs without one, which is about 4x less work with edges that
//! are a little softer.

use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::{GrayImage, RgbImage};
use ort::session::Session;
use ort::value::Tensor;

use crate::{Error, Result};

/// Model files, relative to the models folder. Optional: without either there is no
/// background blur. The 1024 one is for graphics cards, the 512 one for the CPU.
pub const SUBJECT: &str = "birefnet-lite/model.onnx";
pub const SUBJECT_FAST: &str = "birefnet-lite-512/model.onnx";

const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];
/// Longest side of the masks kept.
pub const MASK_MAX: u32 = 1024;

pub struct Segmenter {
    session: Session,
    /// Side of the model's square input.
    size: u32,
}

fn resize_rgb(img: &RgbImage, w: u32, h: u32) -> Result<RgbImage> {
    let mut out = RgbImage::new(w, h);
    let options = ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear));
    Resizer::new().resize(img, &mut out, &options).map_err(|e| Error::Model(format!("cannot resize: {e}")))?;
    Ok(out)
}

impl Segmenter {
    pub fn new(session: Session) -> Segmenter {
        // The input is [1, 3, side, side]; exports have a fixed side.
        let size = session
            .inputs()
            .first()
            .and_then(|i| match i.dtype() {
                ort::value::ValueType::Tensor { shape, .. } => shape.last().copied(),
                _ => None,
            })
            .filter(|s| *s > 0)
            .map(|s| s as u32)
            .unwrap_or(1024);
        Segmenter { session, size }
    }

    pub fn input_size(&self) -> u32 {
        self.size
    }

    /// The subject of `img` (white) on black, the picture's shape, at most [`MASK_MAX`] wide or tall.
    pub fn mask(&mut self, img: &RgbImage) -> Result<GrayImage> {
        let size = self.size;
        let square = resize_rgb(img, size, size)?;
        let plane = (size * size) as usize;
        let mut input = vec![0f32; 3 * plane];
        for (i, p) in square.pixels().enumerate() {
            for c in 0..3 {
                input[c * plane + i] = (p[c] as f32 / 255.0 - MEAN[c]) / STD[c];
            }
        }
        let tensor = Tensor::from_array(([1usize, 3, size as usize, size as usize], input))?;
        let outputs = self.session.run(ort::inputs!["input_image" => tensor])?;
        let (_, logits) = outputs[0].try_extract_tensor::<f32>()?;
        if logits.len() != plane {
            return Err(Error::Model(format!("unexpected mask size {}", logits.len())));
        }
        let matte = GrayImage::from_fn(size, size, |x, y| {
            let v = logits[(y * size + x) as usize];
            image::Luma([(255.0 / (1.0 + (-v).exp())).round() as u8])
        });
        // Back to the picture's shape.
        let (w, h) = (img.width().max(1) as f32, img.height().max(1) as f32);
        let k = MASK_MAX as f32 / w.max(h);
        let (mw, mh) = (((w * k).round() as u32).max(1), ((h * k).round() as u32).max(1));
        Ok(image::imageops::resize(&matte, mw, mh, image::imageops::FilterType::Triangle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bright disc on a plain background is found as the subject.
    #[test]
    fn finds_an_object_in_front() {
        let dirs = crate::model_dirs(&photos_core::storage::home_dir().unwrap());
        for (file, side) in [(SUBJECT, 1024), (SUBJECT_FAST, 512)] {
            let Some(dir) = dirs.iter().find(|d| d.join(file).is_file()) else {
                eprintln!("{file} not installed; skipping");
                continue;
            };
            let started = std::time::Instant::now();
            let (session, _) = crate::open_session_with(&dir.join(file), false, &[], crate::foreground_threads()).unwrap();
            let mut s = Segmenter::new(session);
            let loaded = started.elapsed();
            assert_eq!(s.input_size(), side);
            let started = std::time::Instant::now();
            check(&mut s);
            eprintln!("{file} on the CPU: loaded in {loaded:.1?}, found in {:.1?}", started.elapsed());
        }
    }

    fn check(s: &mut Segmenter) {
        let img = RgbImage::from_fn(600, 400, |x, y| {
            let (dx, dy) = (x as f32 - 300.0, y as f32 - 200.0);
            if dx * dx + dy * dy < 110.0 * 110.0 { image::Rgb([220, 40, 40]) } else { image::Rgb([200, 210, 205]) }
        });
        let mask = s.mask(&img).unwrap();
        assert_eq!((mask.width(), mask.height()), (1024, 683));
        let centre = mask.get_pixel(512, 341)[0];
        let corner = mask.get_pixel(30, 30)[0];
        assert!(centre > 180 && corner < 60, "centre {centre}, corner {corner}");
    }
}
