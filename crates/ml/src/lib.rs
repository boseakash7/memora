//! On-device recognition with ONNX Runtime: faces (YuNet + SFace) and CLIP image/text
//! vectors, and the editor's subject finder. The model files ship with the full installer,
//! or the user picks the features they want and their files are downloaded once from
//! Hugging Face (see [`models`], [`download`]). Photos never leave the PC, and ONNX
//! Runtime's telemetry is switched off.

pub mod clip;
pub mod download;
pub mod faces;
pub mod models;
pub mod scene;
pub mod segment;
pub mod tokenizer;
mod worker;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ort::ep;
use ort::logging::LogLevel;
use ort::session::Session;
use ort::session::builder::GraphOptimizationLevel;

pub use models::Feature;
pub use worker::{Ai, AiStatus, FeatureStatus, Installed, Job, JobState, Prepared, prepare};

pub const YUNET: &str = "face_detection_yunet_2023mar.onnx";
pub const SFACE: &str = "face_recognition_sface_2021dec.onnx";
pub const CLIP_VISION: &str = "clip-vit-base-patch32/vision_model_quantized.onnx";
pub const CLIP_TEXT: &str = "clip-vit-base-patch32/text_model_quantized.onnx";
pub const CLIP_TOKENIZER: &str = "clip-vit-base-patch32/tokenizer.json";
pub const MODEL_FILES: &[&str] = &[YUNET, SFACE, CLIP_VISION, CLIP_TEXT, CLIP_TOKENIZER];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ONNX Runtime: {0}")]
    Ort(String),
    #[error("{0}")]
    Model(String),
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Core(#[from] photos_core::Error),
}

impl<T> From<ort::Error<T>> for Error {
    fn from(e: ort::Error<T>) -> Self {
        Error::Ort(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Where recognition runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Device {
    /// DirectML on the graphics card.
    Gpu,
    Cpu,
}

/// Folders searched for model files, in order: `MEMORA_MODELS_DIR`, `models` next to the
/// program (installed app), then `models` in Memora's home folder (`%LOCALAPPDATA%\Memora`,
/// which stays put when the library data is moved).
pub fn model_dirs(home_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(d) = std::env::var_os("MEMORA_MODELS_DIR") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir.join("models"));
    }
    dirs.push(home_dir.join("models"));
    dirs
}

/// The first folder that has every model file.
pub fn find_models(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().find(|d| MODEL_FILES.iter().all(|f| d.join(f).is_file())).cloned()
}

fn environment() -> Result<&'static ort::environment::Environment> {
    static ENV: OnceLock<std::result::Result<ort::environment::Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| ort::init().with_name("memora").with_telemetry(false).build().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| Error::Ort(e.clone()))
}

/// Whether ONNX Runtime can run models on a graphics card here (DirectML), found by opening
/// a one-node model on it once. Decides which size of a model to download.
pub fn gpu_usable() -> bool {
    static USABLE: OnceLock<bool> = OnceLock::new();
    *USABLE.get_or_init(|| {
        let probe = || -> Result<()> {
            Session::builder(environment()?)?
                .with_log_level(LogLevel::Fatal)?
                .with_memory_pattern(false)?
                .with_execution_providers([ep::DirectML::default().build().error_on_failure()])?
                .commit_from_memory(&identity_model())?;
            Ok(())
        };
        match probe() {
            Ok(()) => true,
            Err(e) => {
                tracing::info!("no usable graphics card: {e}");
                false
            }
        }
    })
}

/// The smallest ONNX model: y = Identity(x) on a float[1], written out as protobuf.
fn identity_model() -> Vec<u8> {
    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    fn num(field: u64, v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        varint(&mut out, field << 3);
        varint(&mut out, v);
        out
    }
    fn bytes(field: u64, b: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        varint(&mut out, (field << 3) | 2);
        varint(&mut out, b.len() as u64);
        out.extend_from_slice(b);
        out
    }
    // ValueInfoProto { name, type: TypeProto { tensor_type { elem_type: FLOAT, shape { dim { dim_value: 1 } } } } }
    let value = |name: &str| {
        let shape = bytes(1, &num(1, 1));
        let tensor = [num(1, 1), bytes(2, &shape)].concat();
        [bytes(1, name.as_bytes()), bytes(2, &bytes(1, &tensor))].concat()
    };
    let node = [bytes(1, b"x"), bytes(2, b"y"), bytes(4, b"Identity")].concat();
    let graph = [bytes(1, &node), bytes(2, b"probe"), bytes(11, &value("x")), bytes(12, &value("y"))].concat();
    let opset = [bytes(1, b""), num(2, 13)].concat();
    [num(1, 8), bytes(8, &opset), bytes(7, &graph)].concat()
}

/// Threads a CPU session may use: few, so the PC stays responsive while it works.
fn cpu_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get() / 4).unwrap_or(2).clamp(1, 4)
}

/// Threads for work someone is waiting for (the editor): all but one core.
pub fn foreground_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get().saturating_sub(1)).unwrap_or(2).max(1)
}

/// Open a model, on the graphics card if wanted and possible, else on the CPU.
/// `sizes` fixes dynamic input dimensions by name, which DirectML runs faster.
pub fn open_session(path: &Path, gpu: bool, sizes: &[(&str, i64)]) -> Result<(Session, Device)> {
    open_session_with(path, gpu, sizes, cpu_threads())
}

/// [`open_session`] with `threads` CPU threads when it runs on the CPU.
pub fn open_session_with(path: &Path, gpu: bool, sizes: &[(&str, i64)], threads: usize) -> Result<(Session, Device)> {
    let env = environment()?;
    if gpu {
        let built = || -> Result<Session> {
            let mut b = Session::builder(env)?.with_log_level(LogLevel::Error)?;
            for (name, size) in sizes {
                b = b.with_dimension_override(*name, *size)?;
            }
            Ok(b.with_optimization_level(GraphOptimizationLevel::Level3)?
                // DirectML needs these two off.
                .with_memory_pattern(false)?
                .with_parallel_execution(false)?
                .with_execution_providers([ep::DirectML::default().build().error_on_failure()])?
                .commit_from_file(path)?)
        };
        match built() {
            Ok(s) => return Ok((s, Device::Gpu)),
            Err(e) => tracing::warn!("{}: graphics card not usable, using the CPU: {e}", path.display()),
        }
    }
    let mut b = Session::builder(env)?.with_log_level(LogLevel::Error)?;
    for (name, size) in sizes {
        b = b.with_dimension_override(*name, *size)?;
    }
    let session = b
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_intra_threads(threads.max(1))?
        .with_inter_threads(1)?
        .commit_from_file(path)?;
    Ok((session, Device::Cpu))
}

/// The models folder for tests, if the models are installed on this PC (tests that need
/// them are skipped otherwise).
#[cfg(test)]
pub(crate) fn test_models() -> Option<PathBuf> {
    let home = photos_core::storage::home_dir().ok()?;
    let found = find_models(&model_dirs(&home));
    if found.is_none() {
        eprintln!("models not installed; skipping");
    }
    found
}

#[cfg(test)]
mod probe {
    /// The hand-written probe model loads (on the CPU, which every PC has).
    #[test]
    fn identity_model_is_valid() {
        let s = ort::session::Session::builder(super::environment().unwrap())
            .unwrap()
            .commit_from_memory(&super::identity_model());
        assert!(s.is_ok(), "{:?}", s.err());
        let _ = super::gpu_usable();
    }
}

#[cfg(test)]
mod recognition_label {
    /// Index snapshots carry `photos_core::ai::RECOGNITION` to say which models made their
    /// faces and CLIP vectors. Changing a model file must change that label.
    #[test]
    fn names_the_model_files() {
        let label = photos_core::ai::RECOGNITION;
        for part in ["yunet-2023mar", "sface-2021dec", "clip-vit-base-patch32", "quantized"] {
            assert!(label.contains(part), "{label} lacks {part}");
        }
        assert!(super::YUNET.contains("yunet_2023mar") && super::SFACE.contains("sface_2021dec"));
        assert!(super::CLIP_VISION.starts_with("clip-vit-base-patch32/") && super::CLIP_VISION.contains("quantized"));
    }
}
