//! Time each model alone: `cargo run -p photos-ml --release --example bench -- <models dir> <cpu|gpu>`
use std::path::PathBuf;
use std::time::Instant;
use ort::value::Tensor;
use photos_ml::{CLIP_VISION, SFACE, YUNET, open_session};

fn main() -> photos_ml::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (dir, gpu) = (PathBuf::from(&a[0]), a[1] == "gpu");
    let sizes = [("batch_size", 1), ("num_channels", 3), ("height", 224), ("width", 224)];
    for (file, name, shape, sz) in [(YUNET, "input", [1usize, 3, 640, 640], &[][..]), (SFACE, "data", [1, 3, 112, 112], &[][..]), (CLIP_VISION, "pixel_values", [1, 3, 224, 224], &sizes[..])] {
        let (mut s, dev) = open_session(&dir.join(file), gpu, sz)?;
        let n: usize = shape.iter().product();
        for i in 0..25 {
            let t = Instant::now();
            let input = Tensor::from_array((shape, vec![0.5f32; n]))?;
            let _ = s.run(ort::inputs![name => input])?;
            if i == 0 || i == 24 { println!("{file} {dev:?} run {i}: {:.1?}", t.elapsed()); }
        }
    }
    Ok(())
}
