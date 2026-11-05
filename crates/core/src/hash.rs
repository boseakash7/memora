//! File hashes for duplicate detection: an exact hash of the bytes and a perceptual hash
//! of the picture.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use image::DynamicImage;
use image::imageops::FilterType;

use crate::Result;

/// BLAKE3 of the whole file, as hex. Two files with the same hash are the same bytes.
pub fn file_blake3(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

/// Side of the image the DCT runs on, and of the low-frequency block that becomes the hash.
const SIZE: usize = 32;
const BLOCK: usize = 8;
/// Pictures with less variation than this (solid colors, black frames) have no useful
/// perceptual hash: every such picture would look like every other.
const MIN_DETAIL: f32 = 2.0;

/// 64-bit perceptual hash (pHash): the low-frequency DCT coefficients of a 32x32 grayscale
/// copy, each as one bit for "above the median". Similar pictures differ in few bits,
/// whatever their size or JPEG quality. None for pictures without detail.
pub fn phash(img: &DynamicImage) -> Option<u64> {
    let gray = img.resize_exact(SIZE as u32, SIZE as u32, FilterType::Triangle).to_luma8();

    // Orthonormal DCT-II basis, first BLOCK frequencies only.
    let basis: Vec<[f32; SIZE]> = (0..BLOCK)
        .map(|k| {
            let scale = if k == 0 { (1.0 / SIZE as f32).sqrt() } else { (2.0 / SIZE as f32).sqrt() };
            std::array::from_fn(|n| {
                scale * (std::f32::consts::PI * (2 * n + 1) as f32 * k as f32 / (2 * SIZE) as f32).cos()
            })
        })
        .collect();

    // rows[y][u] = sum over x of pixel(x, y) * basis[u][x]
    let mut rows = [[0f32; BLOCK]; SIZE];
    for (y, row) in rows.iter_mut().enumerate() {
        for (u, out) in row.iter_mut().enumerate() {
            *out = (0..SIZE).map(|x| gray.get_pixel(x as u32, y as u32).0[0] as f32 * basis[u][x]).sum();
        }
    }
    let mut coeffs = [0f32; BLOCK * BLOCK];
    for v in 0..BLOCK {
        for u in 0..BLOCK {
            coeffs[v * BLOCK + u] = (0..SIZE).map(|y| rows[y][u] * basis[v][y]).sum();
        }
    }

    // Skip the DC term (average brightness) when judging detail and finding the median.
    let mut ac: Vec<f32> = coeffs[1..].to_vec();
    if ac.iter().all(|c| c.abs() < MIN_DETAIL) {
        return None;
    }
    ac.sort_by(|a, b| a.total_cmp(b));
    let median = ac[ac.len() / 2];

    let mut hash = 0u64;
    for (i, c) in coeffs.iter().enumerate().skip(1) {
        if *c > median {
            hash |= 1 << i;
        }
    }
    Some(hash)
}

/// Perceptual hash of an image file (the cached thumbnail, in practice).
pub fn phash_file(path: &Path) -> Result<Option<u64>> {
    let img = image::open(path)?;
    Ok(phash(&img))
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, imageops};

    /// A picture with structure: gradients and a few shapes, so it has real detail.
    fn scene(w: u32, h: u32, seed: u32) -> RgbImage {
        RgbImage::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let a = ((fx * (3.0 + seed as f32) * std::f32::consts::TAU).sin() * 0.5 + 0.5) * 255.0;
            let b = ((fy * (2.0 + seed as f32 * 2.0) * std::f32::consts::TAU).cos() * 0.5 + 0.5) * 255.0;
            let blob = if (fx - 0.3).powi(2) + (fy - 0.6).powi(2) < 0.04 * (1 + seed) as f32 { 255.0 } else { 0.0 };
            Rgb([a as u8, b as u8, blob as u8])
        })
    }

    #[test]
    fn same_picture_at_other_sizes_hashes_alike() {
        let big = DynamicImage::ImageRgb8(scene(1200, 800, 1));
        let small = big.resize_exact(300, 200, FilterType::Lanczos3);
        let blurred = DynamicImage::ImageRgb8(imageops::blur(&big.to_rgb8(), 2.0));
        let (hb, hs, hbl) = (phash(&big).unwrap(), phash(&small).unwrap(), phash(&blurred).unwrap());
        assert!(hamming(hb, hs) <= 3, "resized: {}", hamming(hb, hs));
        assert!(hamming(hb, hbl) <= 6, "blurred: {}", hamming(hb, hbl));
    }

    #[test]
    fn different_pictures_hash_apart() {
        let a = phash(&DynamicImage::ImageRgb8(scene(400, 300, 1))).unwrap();
        let b = phash(&DynamicImage::ImageRgb8(scene(400, 300, 4))).unwrap();
        assert!(hamming(a, b) > 12, "distance {}", hamming(a, b));
    }

    #[test]
    fn flat_pictures_have_no_hash() {
        assert_eq!(phash(&DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 100, Rgb([0, 0, 0])))), None);
        assert_eq!(phash(&DynamicImage::ImageRgb8(RgbImage::from_pixel(100, 100, Rgb([200, 10, 90])))), None);
    }

    #[test]
    fn blake3_matches_content_not_name() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (dir.path().join("a"), dir.path().join("b.jpg"), dir.path().join("c"));
        std::fs::write(&a, b"same bytes").unwrap();
        std::fs::write(&b, b"same bytes").unwrap();
        std::fs::write(&c, b"other bytes").unwrap();
        assert_eq!(file_blake3(&a).unwrap(), file_blake3(&b).unwrap());
        assert_ne!(file_blake3(&a).unwrap(), file_blake3(&c).unwrap());
        assert_eq!(file_blake3(&a).unwrap().len(), 64);
        assert!(file_blake3(&dir.path().join("missing")).is_err());
    }
}
