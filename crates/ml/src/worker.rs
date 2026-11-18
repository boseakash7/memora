//! The recognition worker. Once indexing is done it looks at each item once: faces (grouped
//! into people now and then) and a CLIP vector for searching by content. It also turns
//! search texts into vectors.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use image::RgbImage;
use photos_core::ai::{self, Found, FoundFace};
use photos_core::media::Media;
use photos_core::{Library, people, settings, thumbs};
use rayon::prelude::*;
use serde::Serialize;

use crate::clip::{self, ImageEncoder, TextEncoder};
use crate::faces::{self, Detector, Embedder};
use crate::{CLIP_TEXT, CLIP_TOKENIZER, CLIP_VISION, Device, Result, SFACE, YUNET, find_models, open_session};

/// Items looked at per round.
const BATCH: usize = 8;
/// At most this many faces per picture (crowds).
const MAX_FACES: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Starting,
    /// Switched off in Settings.
    Off,
    /// The model files are not installed.
    NoModels,
    /// Waiting for indexing to finish first.
    Waiting,
    Working,
    /// Everything has been looked at.
    Idle,
    /// The models could not be loaded; see `error`.
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiStatus {
    pub enabled: bool,
    pub state: State,
    pub device: Option<Device>,
    pub error: Option<String>,
    /// Folder the models were loaded from.
    pub models_dir: Option<PathBuf>,
    /// Folders searched for the models.
    pub searched: Vec<PathBuf>,
    /// Items still to look at.
    pub pending: u64,
}

/// What the worker is doing: state, device, error, models folder.
type Current = (State, Option<Device>, Option<String>, Option<PathBuf>);

/// Handle to the recognition worker. Cheap to clone.
#[derive(Clone)]
pub struct Ai {
    inner: Arc<Inner>,
}

struct Inner {
    library: Library,
    dirs: Vec<PathBuf>,
    state: Mutex<Current>,
    wake: (Mutex<bool>, Condvar),
    /// Search side: the text model (on the CPU, loaded on first search) and the vector of
    /// the neutral text that searches are measured against.
    text: Mutex<Option<(TextEncoder, Vec<f32>)>>,
}

impl Ai {
    /// Start the worker. Models are looked for in `dirs` (see [`crate::model_dirs`]).
    pub fn start(library: Library, dirs: Vec<PathBuf>) -> Ai {
        let inner = Arc::new(Inner {
            library: library.clone(),
            dirs,
            state: Mutex::new((State::Starting, None, None, None)),
            wake: (Mutex::new(false), Condvar::new()),
            text: Mutex::new(None),
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        library.subscribe(move |_| {
            if let Some(i) = weak.upgrade() {
                i.wake();
            }
        });
        let worker = inner.clone();
        if let Err(e) = std::thread::Builder::new().name("ai-worker".into()).spawn(move || worker.run()) {
            tracing::error!("cannot start recognition: {e}");
        }
        Ai { inner }
    }

    pub fn status(&self) -> AiStatus {
        let (state, device, error, models_dir) = self.inner.state.lock().unwrap().clone();
        let enabled = settings::load(self.inner.library.db()).map(|s| s.ml_enabled).unwrap_or(false);
        AiStatus {
            enabled,
            state,
            device,
            error,
            models_dir,
            searched: self.inner.dirs.clone(),
            pending: self.inner.library.status().ai_pending,
        }
    }

    /// Settings changed (on/off, GPU) or model files were added: look again.
    pub fn refresh(&self) {
        self.inner.wake();
    }

    /// Items whose picture matches the text, best first. Empty when recognition is off or
    /// the models are missing.
    pub fn search(&self, text: &str) -> Result<Vec<(i64, f32)>> {
        let lib = &self.inner.library;
        if text.trim().is_empty() || !settings::load(lib.db())?.ml_enabled {
            return Ok(Vec::new());
        }
        let Some(dir) = find_models(&self.inner.dirs) else { return Ok(Vec::new()) };
        let v = {
            let mut text_model = self.inner.text.lock().unwrap();
            if text_model.is_none() {
                let (session, _) = open_session(&dir.join(CLIP_TEXT), false, &[])?;
                let mut encoder = TextEncoder::new(session, &dir.join(CLIP_TOKENIZER))?;
                let neutral = encoder.embed("")?;
                *text_model = Some((encoder, neutral));
            }
            let (encoder, neutral) = text_model.as_mut().expect("loaded above");
            let mut v = encoder.embed(text)?;
            v.iter_mut().zip(neutral.iter()).for_each(|(a, b)| *a -= b);
            v
        };
        Ok(lib.semantic().search(lib.db(), &v)?)
    }
}

/// The models the worker runs.
struct Models {
    detector: Detector,
    embedder: Embedder,
    clip: ImageEncoder,
    device: Device,
}

impl Models {
    fn open(dir: &Path, gpu: bool) -> Result<Models> {
        let (d, d1) = open_session(&dir.join(YUNET), gpu, &[])?;
        let (e, d2) = open_session(&dir.join(SFACE), gpu, &[])?;
        let sizes = [("batch_size", 1), ("num_channels", 3), ("height", 224), ("width", 224)];
        let (c, d3) = open_session(&dir.join(CLIP_VISION), gpu, &sizes)?;
        let device = if [d1, d2, d3].contains(&Device::Gpu) { Device::Gpu } else { Device::Cpu };
        Ok(Models { detector: Detector::new(d), embedder: Embedder::new(e), clip: ImageEncoder::new(c), device })
    }

    /// What is in the picture; also hands the picture back.
    fn recognize(&mut self, p: Prepared) -> Result<(Found, RgbImage)> {
        let img = &p.img;
        let (w, h) = (img.width() as f32, img.height() as f32);
        let mut faces = Vec::new();
        for d in self.detector.detect(&p.faces)?.iter().take(MAX_FACES) {
            let [x, y, fw, fh] = d.rect;
            faces.push(FoundFace { rect: [x / w, y / h, fw / w, fh / h], score: d.score, embedding: self.embedder.embed(img, d)? });
        }
        let clip = self.clip.embed(p.clip)?;
        Ok((Found { faces, clip: Some(clip) }, p.img))
    }
}

/// An item's picture with the models' inputs, made on the decoding threads.
pub struct Prepared {
    pub img: RgbImage,
    pub faces: faces::Prepared,
    pub clip: Vec<f32>,
}

pub fn prepare(img: RgbImage) -> Result<Prepared> {
    Ok(Prepared { faces: faces::prepare(&img)?, clip: clip::prepare(&img)?, img })
}

impl Inner {
    fn wake(&self) {
        let (lock, cvar) = &self.wake;
        *lock.lock().unwrap() = true;
        cvar.notify_one();
    }

    fn wait(&self, max: Duration) {
        let (lock, cvar) = &self.wake;
        let mut woken = lock.lock().unwrap();
        if !*woken {
            woken = cvar.wait_timeout(woken, max).unwrap().0;
        }
        *woken = false;
    }

    fn set(&self, state: State, device: Option<Device>, error: Option<String>, dir: Option<PathBuf>) {
        let mut s = self.state.lock().unwrap();
        if s.0 != state {
            tracing::info!("recognition: {state:?}{}", error.as_deref().map(|e| format!(" ({e})")).unwrap_or_default());
        }
        *s = (state, device, error, dir);
    }

    fn run(&self) {
        let lib = &self.library;
        // Background work: below-normal priority, also for the FFmpeg runs of video frames.
        photos_core::ffmpeg::mark_background_thread();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .thread_name(|i| format!("ai-decode-{i}"))
            .start_handler(|_| photos_core::ffmpeg::mark_background_thread())
            .build()
            .expect("decode pool");
        // Loaded models and whether they were opened for the GPU.
        let mut models: Option<(Models, bool)> = None;
        let mut gpu_failed = false;
        let mut new_faces = 0usize;

        while !lib.is_stopped() {
            let s = settings::load(lib.db()).unwrap_or_default();
            if !s.ml_enabled {
                models = None;
                self.set(State::Off, None, None, None);
                self.wait(Duration::from_secs(60));
                continue;
            }
            let Some(dir) = find_models(&self.dirs) else {
                models = None;
                self.set(State::NoModels, None, None, None);
                self.wait(Duration::from_secs(60));
                continue;
            };
            let status = lib.status();
            if status.pending > 0 || status.scanning {
                self.set(State::Waiting, models.as_ref().map(|m| m.0.device), None, Some(dir));
                self.wait(Duration::from_secs(5));
                continue;
            }
            let want_gpu = s.ml_use_gpu && !gpu_failed;
            if models.as_ref().map(|m| m.1) != Some(want_gpu) {
                models = None;
                match Models::open(&dir, want_gpu) {
                    Ok(m) => {
                        tracing::info!("recognition models loaded from {} ({:?})", dir.display(), m.device);
                        models = Some((m, want_gpu));
                    }
                    Err(e) => {
                        self.set(State::Error, None, Some(e.to_string()), Some(dir));
                        self.wait(Duration::from_secs(60));
                        continue;
                    }
                }
            }
            let (m, on_gpu) = models.as_mut().expect("loaded above");
            let device = m.device;

            let batch = match ai::next_batch(lib.db(), BATCH) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!("cannot read items to recognise: {e}");
                    self.wait(Duration::from_secs(10));
                    continue;
                }
            };
            if batch.is_empty() {
                if new_faces > 0 {
                    self.group();
                    new_faces = 0;
                }
                self.set(State::Idle, Some(device), None, Some(dir));
                self.wait(Duration::from_secs(300));
                continue;
            }
            self.set(State::Working, Some(device), None, Some(dir));

            let paths = lib.paths();
            let started = Instant::now();
            let prepared: Vec<_> = pool.install(|| {
                batch.par_iter().map(|item| thumbs::analysis_image(paths, item).map_err(crate::Error::from).and_then(prepare)).collect()
            });
            let mut failed_on_gpu = false;
            for (item, p) in batch.iter().zip(prepared) {
                // Kept for the face pictures.
                let mut img = None;
                let found = match p {
                    Ok(p) => match m.recognize(p).map(|(f, i)| {
                        img = Some(i);
                        f
                    }) {
                        Ok(f) => f,
                        Err(e) if *on_gpu && device == Device::Gpu => {
                            tracing::warn!("recognition failed on the graphics card, switching to the CPU: {e}");
                            failed_on_gpu = true;
                            break;
                        }
                        Err(e) => {
                            tracing::warn!("{}: recognition failed: {e}", item.path.display());
                            Found::default()
                        }
                    },
                    // Unreadable file: nothing to find (the index already shows the error).
                    Err(_) => Found::default(),
                };
                new_faces += self.store(item, &found, img.as_ref());
            }
            if failed_on_gpu {
                gpu_failed = true;
                models = None;
                continue;
            }
            tracing::debug!("recognised {} items in {:.1?}", batch.len(), started.elapsed());

            let ungrouped = people::ungrouped_count(lib.db()).unwrap_or(0) as usize;
            if new_faces >= 200.max(ungrouped / 10) {
                self.group();
                new_faces = 0;
            } else {
                lib.report_ai(false);
            }
        }
    }

    /// Save one item's results. Returns how many faces were stored.
    fn store(&self, item: &Media, found: &Found, img: Option<&RgbImage>) -> usize {
        let lib = &self.library;
        match ai::save(lib.db(), item, found) {
            Ok(Some(saved)) => {
                for id in &saved.removed_faces {
                    let _ = std::fs::remove_file(thumbs::face_path(lib.paths(), *id));
                }
                if let Some(img) = img {
                    for (id, face) in saved.face_ids.iter().zip(&found.faces) {
                        if let Err(e) = thumbs::write_face(lib.paths(), *id, img, face.rect) {
                            tracing::debug!("cannot save face picture: {e}");
                        }
                    }
                }
                if let Some(v) = &found.clip {
                    lib.semantic().put(item.id, v);
                }
                saved.face_ids.len()
            }
            Ok(None) => 0, // changed on disk meanwhile; it comes round again
            Err(e) => {
                tracing::error!("cannot save recognition results: {e}");
                std::thread::sleep(Duration::from_secs(1));
                0
            }
        }
    }

    fn group(&self) {
        let started = Instant::now();
        match people::cluster(self.library.db()) {
            Ok(stats) => tracing::info!("grouped faces in {:.1?}: {stats:?}", started.elapsed()),
            Err(e) => tracing::error!("cannot group faces: {e}"),
        }
        self.library.report_ai(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photos_core::{AppPaths, Db};

    #[test]
    fn looks_at_items_after_indexing_and_searches() {
        let Some(models) = crate::test_models() else { return };
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Photos");
        std::fs::create_dir_all(&root).unwrap();
        // Big enough to be searched in quarters too.
        image::RgbImage::from_fn(1400, 1000, |x, y| image::Rgb([(x / 6) as u8, (y / 4) as u8, 200]))
            .save(root.join("gradient.jpg"))
            .unwrap();
        image::RgbImage::from_pixel(300, 300, image::Rgb([220, 30, 30])).save(root.join("red.png")).unwrap();

        let paths = AppPaths::in_dir(&dir.path().join("data")).unwrap();
        let lib = Library::start(Db::open(&paths.db_path).unwrap(), paths).unwrap();
        let ai = Ai::start(lib.clone(), vec![models]);
        lib.add_folder(&root, false).unwrap();

        let start = Instant::now();
        loop {
            let s = ai.status();
            if lib.status().total == 2 && s.pending == 0 && s.state == State::Idle {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(120), "recognition finished: {s:?}");
            std::thread::sleep(Duration::from_millis(100));
        }
        let conn = lib.db().conn().unwrap();
        let clips: i64 = conn.query_row("SELECT count(*) FROM clip_embeddings", [], |r| r.get(0)).unwrap();
        let faces: i64 = conn.query_row("SELECT count(*) FROM faces", [], |r| r.get(0)).unwrap();
        assert_eq!((clips, faces), (2, 0));
        let status = ai.status();
        assert!(status.device.is_some() && status.models_dir.is_some(), "{status:?}");

        // The red picture matches "red" better than the gradient does.
        let hits = ai.search("a plain red square").unwrap();
        let red = conn.query_row("SELECT id FROM media WHERE name = 'red.png'", [], |r| r.get::<_, i64>(0)).unwrap();
        if let Some(first) = hits.first() {
            assert_eq!(first.0, red, "{hits:?}");
        }
        assert!(ai.search("   ").unwrap().is_empty());
        lib.shutdown();
    }
}
