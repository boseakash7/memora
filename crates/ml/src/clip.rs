//! CLIP ViT-B/32: pictures and texts become 512-number vectors that point the same way
//! when the text describes the picture. Preprocessing follows Hugging Face's
//! `CLIPImageProcessor` (shortest side to 224, centre crop, normalise).

use std::path::Path;

use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;

use crate::tokenizer::Tokenizer;
use crate::Result;

const SIZE: u32 = 224;
const MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
const STD: [f32; 3] = [0.268_629_54, 0.261_302_6, 0.275_777_1];

pub struct ImageEncoder {
    session: Session,
}

impl ImageEncoder {
    pub fn new(session: Session) -> ImageEncoder {
        ImageEncoder { session }
    }

    /// Vector for a picture made by [`prepare`].
    pub fn embed(&mut self, input: Vec<f32>) -> Result<Vec<f32>> {
        let tensor = Tensor::from_array(([1usize, 3, SIZE as usize, SIZE as usize], input))?;
        let outputs = self.session.run(ort::inputs!["pixel_values" => tensor])?;
        let mut v = outputs[0].try_extract_tensor::<f32>()?.1.to_vec();
        photos_core::people::normalize(&mut v);
        Ok(v)
    }
}

/// The model's input for a picture: normalised RGB planes of its centre square, 224x224.
pub fn prepare(img: &RgbImage) -> Result<Vec<f32>> {
    let (w, h) = img.dimensions();
    let side = w.min(h).max(1) as f64;
    let mut crop = RgbImage::new(SIZE, SIZE);
    let options = ResizeOptions::new()
        .crop((w as f64 - side) / 2.0, (h as f64 - side) / 2.0, side, side)
        .resize_alg(ResizeAlg::Convolution(FilterType::CatmullRom));
    Resizer::new().resize(img, &mut crop, &options).map_err(|e| crate::Error::Model(format!("cannot resize: {e}")))?;
    let plane = (SIZE * SIZE) as usize;
    let mut out = vec![0f32; 3 * plane];
    for (x, y, p) in crop.enumerate_pixels() {
        let i = (y * SIZE + x) as usize;
        for c in 0..3 {
            out[c * plane + i] = (p[c] as f32 / 255.0 - MEAN[c]) / STD[c];
        }
    }
    Ok(out)
}

pub struct TextEncoder {
    session: Session,
    tokenizer: Tokenizer,
}

impl TextEncoder {
    pub fn new(session: Session, tokenizer: &Path) -> Result<TextEncoder> {
        Ok(TextEncoder { session, tokenizer: Tokenizer::open(tokenizer)? })
    }

    /// Vector for a search: "dog" is read as "a photo of dog", which CLIP matches better.
    pub fn embed(&mut self, text: &str) -> Result<Vec<f32>> {
        let ids = self.tokenizer.encode(&format!("a photo of {}", text.trim()));
        let tensor = Tensor::from_array(([1usize, ids.len()], ids))?;
        let outputs = self.session.run(ort::inputs!["input_ids" => tensor])?;
        let mut v = outputs[0].try_extract_tensor::<f32>()?.1.to_vec();
        photos_core::people::normalize(&mut v);
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_the_centre_square() {
        // Left third red, middle blue, right third green: the centre crop is mostly blue.
        let img = RgbImage::from_fn(900, 300, |x, _| match x / 300 {
            0 => image::Rgb([255, 0, 0]),
            1 => image::Rgb([0, 0, 255]),
            _ => image::Rgb([0, 255, 0]),
        });
        let px = prepare(&img).unwrap();
        let plane = (SIZE * SIZE) as usize;
        let centre = (112 * SIZE + 112) as usize;
        let blue = (1.0 - MEAN[2]) / STD[2];
        assert!((px[2 * plane + centre] - blue).abs() < 0.01);
        assert_eq!(px.len(), 3 * plane);
    }
}
