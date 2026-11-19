//! Filters side by side on library photos, for tuning `edit::filters`:
//! `cargo run -p photos-core --example look_sheet -- <data dir> <out.jpg> <filter,...> <media id,...>`
//! One row per photo; the first column is the photo as it is. `-` in the filter list is a gap.
use std::path::{Path, PathBuf};

use image::{GenericImage, RgbImage};
use photos_core::edit::{Edit, render};

const TILE: u32 = 300;

fn source(data: &Path, id: i64) -> Option<RgbImage> {
    for kind in ["display", "thumb"] {
        let p = data.join("cache").join(kind).join((id / 1000).to_string()).join(format!("{id}.jpg"));
        if let Ok(img) = image::open(p) {
            return Some(img.to_rgb8());
        }
    }
    None
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (data, out) = (PathBuf::from(&a[0]), PathBuf::from(&a[1]));
    let filters: Vec<Option<String>> =
        std::iter::once(None).chain(a[2].split(',').map(|f| Some(f.to_string()).filter(|f| f != "-"))).collect();
    let ids: Vec<i64> = a[3].split(',').map(|s| s.parse().unwrap()).collect();
    for f in filters.iter().flatten() {
        assert!(photos_core::edit::filters::find(f).is_some(), "no filter {f}");
    }
    let mut sheet = RgbImage::from_pixel(filters.len() as u32 * TILE, ids.len() as u32 * TILE, image::Rgb([24, 24, 24]));
    for (row, id) in ids.iter().enumerate() {
        let Some(src) = source(&data, *id) else {
            eprintln!("no cached copy of {id}");
            continue;
        };
        for (col, f) in filters.iter().enumerate() {
            if col > 0 && f.is_none() {
                continue;
            }
            let edit = Edit { filter: f.clone(), ..Edit::default() };
            let img = render::render(&src, None, &edit, TILE - 4, TILE - 4);
            let (x, y) = (col as u32 * TILE, row as u32 * TILE);
            sheet.copy_from(&img, x + (TILE - img.width()) / 2, y + (TILE - img.height()) / 2).unwrap();
        }
    }
    sheet.save(&out).unwrap();
    println!("{}", out.display());
}
