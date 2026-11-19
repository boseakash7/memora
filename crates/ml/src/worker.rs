//! The recognition worker. Once indexing is done it looks at each item once: faces (grouped
//! into people now and then) and a CLIP vector for searching by content. It also turns
//! search texts into vectors.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use image::RgbImage;
use photos_core::ai::{self, Found, FoundFace};
use photos_core::media::Media;
use photos_core::{Library, people, settings, thumbs};
use rayon::prelude::*;
use serde::Serialize;

use crate::clip::{self, ImageEncoder, TextEncoder};
use crate::scene;
use crate::segment::{SUBJECT, SUBJECT_FAST, Segmenter};
use image::GrayImage;
use photos_core::edit::filters::Scene;
use crate::download::{self, DownloadError};
use crate::faces::{self, Detector, Embedder};
use crate::models::{self, Feature};
use crate::{CLIP_TEXT, CLIP_TOKENIZER, CLIP_VISION, Device, Result, SFACE, YUNET, open_session, open_session_with};

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
    /// Which features' models are installed.
    pub installed: Installed,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Installed {
    pub faces: bool,
    pub search: bool,
    pub blur: bool,
}

/// A feature's models: installed or not, and its download if one is going.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureStatus {
    pub feature: Feature,
    pub installed: bool,
    /// Downloaded here, so it can be removed (not part of the installed app).
    pub removable: bool,
    /// Bytes on disk when installed, else bytes to download.
    pub size: u64,
    /// For blur: the full-size model (for a graphics card) rather than the lighter one.
    pub full: bool,
    pub job: Option<Job>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub state: JobState,
    pub got: u64,
    pub total: u64,
    pub error: Option<String>,
    #[serde(skip)]
    full: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Queued,
    Downloading,
    /// Stopped by the user; what came down is kept for later.
    Paused,
    Failed,
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
    /// The editor's subject finder (loaded on first use) and the scene texts' vectors.
    subject: Mutex<Option<(Segmenter, Device)>>,
    scenes: Mutex<Option<SceneTexts>>,
    /// Where downloaded models go (the last of `dirs`, in Memora's home folder).
    download_dir: PathBuf,
    jobs: Mutex<HashMap<Feature, Job>>,
    /// Features waiting to download, and whether the download thread is running.
    queue: Mutex<(VecDeque<Feature>, bool)>,
    stops: Mutex<HashMap<Feature, Arc<AtomicBool>>>,
}

/// The vector of each text in [`scene::all_texts`].
type SceneTexts = Vec<Vec<f32>>;

impl Ai {
    /// Start the worker. Models are looked for in `dirs` (see [`crate::model_dirs`]).
    pub fn start(library: Library, dirs: Vec<PathBuf>) -> Ai {
        let download_dir = dirs.last().cloned().unwrap_or_else(|| PathBuf::from("models"));
        let inner = Arc::new(Inner {
            library: library.clone(),
            dirs,
            download_dir,
            jobs: Mutex::new(HashMap::new()),
            queue: Mutex::new((VecDeque::new(), false)),
            stops: Mutex::new(HashMap::new()),
            state: Mutex::new((State::Starting, None, None, None)),
            wake: (Mutex::new(false), Condvar::new()),
            text: Mutex::new(None),
            subject: Mutex::new(None),
            scenes: Mutex::new(None),
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
            installed: self.installed(),
        }
    }

    pub fn installed(&self) -> Installed {
        let dirs = &self.inner.dirs;
        Installed {
            faces: Feature::Faces.installed_in(dirs).is_some(),
            search: Feature::Search.installed_in(dirs).is_some(),
            blur: Feature::Blur.installed_in(dirs).is_some(),
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
        let Some(dir) = Feature::Search.installed_in(&self.inner.dirs) else { return Ok(Vec::new()) };
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

impl Ai {
    /// The folder holding the subject model, if installed.
    fn subject_file(&self, name: &str) -> Option<PathBuf> {
        self.inner.dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())
    }

    /// Whether the editor can blur backgrounds (a subject model is installed).
    pub fn can_find_subjects(&self) -> bool {
        self.subject_file(SUBJECT).is_some() || self.subject_file(SUBJECT_FAST).is_some()
    }

    fn gpu_wanted(&self) -> bool {
        settings::load(self.inner.library.db()).map(|s| s.ml_use_gpu).unwrap_or(true)
    }

    /// Whether finding subjects runs on the graphics card: known once it ran, else a guess
    /// from the setting and from where recognition runs. None when nothing tells yet.
    pub fn subjects_on_gpu(&self) -> Option<bool> {
        if let Some((_, device)) = self.inner.subject.lock().unwrap().as_ref() {
            return Some(*device == Device::Gpu);
        }
        if !self.gpu_wanted() {
            return Some(false);
        }
        self.inner.state.lock().unwrap().1.map(|d| d == Device::Gpu)
    }

    /// Open the subject model: the full-size one on the graphics card when there is one,
    /// else the 512 one on the CPU, using all but one core (someone is waiting for it).
    fn open_subject(&self) -> Result<(Segmenter, Device)> {
        let (full, fast) = (self.subject_file(SUBJECT), self.subject_file(SUBJECT_FAST));
        let threads = crate::foreground_threads();
        if self.gpu_wanted()
            && let Some(path) = full.clone().or_else(|| fast.clone())
        {
            let (session, device) = open_session_with(&path, true, &[], threads)?;
            if device == Device::Gpu || fast.is_none() || Some(&path) == fast.as_ref() {
                return Ok((Segmenter::new(session), device));
            }
            // No usable graphics card: the smaller model is much quicker on the CPU.
        }
        let path = fast.or(full).ok_or_else(|| crate::Error::Model("the subject model is not installed".into()))?;
        let (session, device) = open_session_with(&path, false, &[], threads)?;
        Ok((Segmenter::new(session), device))
    }

    /// The main subject of a picture, white on black (see [`crate::segment`]). About a
    /// second on a graphics card, a few on the CPU; loading the model the first time adds more.
    pub fn subject_mask(&self, img: &RgbImage) -> Result<GrayImage> {
        let mut guard = self.inner.subject.lock().unwrap();
        if guard.is_none() {
            let started = Instant::now();
            let (seg, device) = self.open_subject()?;
            tracing::info!("subject model loaded ({device:?}, {} px) in {:.1?}", seg.input_size(), started.elapsed());
            *guard = Some((seg, device));
        }
        let started = Instant::now();
        let mask = guard.as_mut().expect("loaded above").0.mask(img);
        tracing::info!("subject found in {:.1?}", started.elapsed());
        mask
    }

    /// What kind of scene an item shows, from its stored CLIP vector. None until recognition
    /// has looked at it (or when the models are missing).
    pub fn scene(&self, media_id: i64) -> Result<Option<Scene>> {
        let lib = &self.inner.library;
        let Some(v) = lib.semantic().vector(lib.db(), media_id)? else { return Ok(None) };
        let Some(dir) = Feature::Search.installed_in(&self.inner.dirs) else { return Ok(None) };
        let mut scenes = self.inner.scenes.lock().unwrap();
        if scenes.is_none() {
            let mut text_model = self.inner.text.lock().unwrap();
            if text_model.is_none() {
                let (session, _) = open_session(&dir.join(CLIP_TEXT), false, &[])?;
                let mut encoder = TextEncoder::new(session, &dir.join(CLIP_TOKENIZER))?;
                let neutral = encoder.embed("")?;
                *text_model = Some((encoder, neutral));
            }
            let (encoder, _) = text_model.as_mut().expect("loaded above");
            let mut list = Vec::new();
            for text in scene::all_texts() {
                list.push(encoder.embed(text)?);
            }
            *scenes = Some(list);
        }
        let people_in_it = photos_core::ai::has_faces(lib.db(), media_id)?;
        let scores: Vec<f32> = scenes.as_ref().expect("made above").iter().map(|t| people::dot(&v, t)).collect();
        Ok(scene::pick(&scores, people_in_it))
    }
}

/// Downloading and removing models.
impl Ai {
    /// Whether blur would be downloaded in its full size: when the graphics card is to be
    /// used and works (or the size an unfinished download already has).
    fn blur_full(&self) -> bool {
        if let Some(job) = self.inner.jobs.lock().unwrap().get(&Feature::Blur) {
            return job.full;
        }
        self.gpu_wanted() && crate::gpu_usable()
    }

    /// Each feature: installed, or what downloading it takes.
    pub fn features(&self) -> Vec<FeatureStatus> {
        let inner = &self.inner;
        let jobs = inner.jobs.lock().unwrap().clone();
        Feature::ALL
            .iter()
            .map(|&f| {
                let found = f.installed_in(&inner.dirs);
                let full = match (f, &found) {
                    (Feature::Blur, Some(d)) => models::has(d, models::BLUR_FULL),
                    (Feature::Blur, None) => self.blur_full(),
                    _ => false,
                };
                FeatureStatus {
                    feature: f,
                    installed: found.is_some(),
                    removable: found.as_ref().is_some_and(|d| *d == inner.download_dir),
                    size: models::total_size(f.files(full)),
                    full,
                    job: jobs.get(&f).cloned(),
                }
            })
            .collect()
    }

    /// Start (or go on with) downloading a feature's models. Nothing happens when they are
    /// installed or already coming down.
    pub fn download(&self, f: Feature) {
        let inner = &self.inner;
        if f.installed_in(&inner.dirs).is_some() {
            return;
        }
        let full = f == Feature::Blur && self.blur_full();
        let files = f.files(full);
        {
            let mut jobs = inner.jobs.lock().unwrap();
            if jobs.get(&f).is_some_and(|j| matches!(j.state, JobState::Queued | JobState::Downloading)) {
                return;
            }
            let got = files.iter().map(|m| download::have(&inner.download_dir, m)).sum();
            jobs.insert(f, Job { state: JobState::Queued, got, total: models::total_size(files), error: None, full });
        }
        let start = {
            let mut q = inner.queue.lock().unwrap();
            if !q.0.contains(&f) {
                q.0.push_back(f);
            }
            !std::mem::replace(&mut q.1, true)
        };
        if start {
            let me = self.clone();
            if let Err(e) = std::thread::Builder::new().name("model-download".into()).spawn(move || me.download_all()) {
                tracing::error!("cannot start downloading: {e}");
                inner.queue.lock().unwrap().1 = false;
            }
        }
    }

    /// Stop a download, keeping what came down so it can go on later.
    pub fn pause(&self, f: Feature) {
        let inner = &self.inner;
        inner.queue.lock().unwrap().0.retain(|q| *q != f);
        if let Some(stop) = inner.stops.lock().unwrap().get(&f) {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(job) = inner.jobs.lock().unwrap().get_mut(&f) {
            job.state = JobState::Paused;
        }
    }

    /// Stop a download and throw away what came down.
    pub fn cancel(&self, f: Feature) {
        self.pause(f);
        self.inner.jobs.lock().unwrap().remove(&f);
        // The download thread may still be writing; it stops at its next chunk.
        let dir = self.inner.download_dir.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            for m in f.files(true).iter().chain(f.files(false)) {
                download::discard(&dir, m);
            }
        });
    }

    /// Delete a feature's downloaded models; the feature turns off.
    pub fn remove(&self, f: Feature) -> std::io::Result<()> {
        self.cancel(f);
        self.forget(f);
        for m in f.files(true).iter().chain(f.files(false)) {
            let p = self.inner.download_dir.join(m.path);
            if p.is_file() {
                std::fs::remove_file(&p)?;
            }
        }
        self.inner.wake();
        Ok(())
    }

    /// Drop the loaded copies of a feature's models (they changed on disk).
    fn forget(&self, f: Feature) {
        match f {
            Feature::Search => {
                *self.inner.scenes.lock().unwrap() = None;
                *self.inner.text.lock().unwrap() = None;
            }
            Feature::Blur => *self.inner.subject.lock().unwrap() = None,
            Feature::Faces => {}
        }
    }

    /// The download thread: one feature after another until the queue is empty.
    fn download_all(&self) {
        let inner = &self.inner;
        let agent = download::agent();
        loop {
            let next = {
                let mut q = inner.queue.lock().unwrap();
                let next = q.0.pop_front();
                if next.is_none() {
                    q.1 = false;
                }
                next
            };
            let Some(f) = next else { return };
            let full = {
                let mut jobs = inner.jobs.lock().unwrap();
                jobs.get_mut(&f).map(|j| {
                    j.state = JobState::Downloading;
                    j.error = None;
                    j.full
                })
            };
            let Some(full) = full else { continue };
            let stop = Arc::new(AtomicBool::new(false));
            inner.stops.lock().unwrap().insert(f, stop.clone());
            let started = Instant::now();
            let mut result = Ok(());
            let mut before = 0;
            for m in f.files(full) {
                let mut report = |n: u64| {
                    if let Some(j) = inner.jobs.lock().unwrap().get_mut(&f) {
                        j.got = before + n;
                    }
                };
                result = download::fetch(&agent, &inner.download_dir, m, &stop, &mut report);
                if result.is_err() {
                    break;
                }
                before += m.size;
            }
            inner.stops.lock().unwrap().remove(&f);
            match result {
                Ok(()) => {
                    inner.jobs.lock().unwrap().remove(&f);
                    tracing::info!("downloaded the {f:?} models in {:.0?}", started.elapsed());
                    self.forget(f);
                    inner.wake();
                }
                Err(DownloadError::Stopped) => {}
                Err(e) => {
                    tracing::warn!("downloading the {f:?} models failed: {e}");
                    if let Some(j) = inner.jobs.lock().unwrap().get_mut(&f) {
                        j.state = JobState::Failed;
                        j.error = Some(e.to_string());
                    }
                }
            }
        }
    }
}

/// The models the worker runs.
/// Each part runs only when its models are installed (see [`Parts`]).
struct Models {
    faces: Option<(Detector, Embedder)>,
    clip: Option<ImageEncoder>,
    device: Device,
}

/// Where each part's models are, if installed.
#[derive(Debug, Clone, PartialEq)]
struct Parts {
    faces: Option<PathBuf>,
    clip: Option<PathBuf>,
}

impl Parts {
    fn find(dirs: &[PathBuf]) -> Parts {
        Parts { faces: models::find(dirs, models::FACES), clip: models::find(dirs, models::SEARCH) }
    }

    /// As bits of `ai::FACES`, `ai::CLIP`.
    fn bits(&self) -> i64 {
        (if self.faces.is_some() { ai::FACES } else { 0 }) | (if self.clip.is_some() { ai::CLIP } else { 0 })
    }
}

impl Models {
    fn open(parts: &Parts, gpu: bool) -> Result<Models> {
        let mut devices = Vec::new();
        let faces = match &parts.faces {
            Some(dir) => {
                let (d, d1) = open_session(&dir.join(YUNET), gpu, &[])?;
                let (e, d2) = open_session(&dir.join(SFACE), gpu, &[])?;
                devices.extend([d1, d2]);
                Some((Detector::new(d), Embedder::new(e)))
            }
            None => None,
        };
        let clip = match &parts.clip {
            Some(dir) => {
                let sizes = [("batch_size", 1), ("num_channels", 3), ("height", 224), ("width", 224)];
                let (c, d3) = open_session(&dir.join(CLIP_VISION), gpu, &sizes)?;
                devices.push(d3);
                Some(ImageEncoder::new(c))
            }
            None => None,
        };
        let device = if devices.contains(&Device::Gpu) { Device::Gpu } else { Device::Cpu };
        Ok(Models { faces, clip, device })
    }

    fn bits(&self) -> i64 {
        (if self.faces.is_some() { ai::FACES } else { 0 }) | (if self.clip.is_some() { ai::CLIP } else { 0 })
    }

    /// What is in the picture; also hands the picture back.
    fn recognize(&mut self, p: Prepared) -> Result<(Found, RgbImage)> {
        let img = &p.img;
        let (w, h) = (img.width() as f32, img.height() as f32);
        let mut faces = Vec::new();
        if let Some((detector, embedder)) = &mut self.faces {
            for d in detector.detect(&p.faces)?.iter().take(MAX_FACES) {
                let [x, y, fw, fh] = d.rect;
                faces.push(FoundFace { rect: [x / w, y / h, fw / w, fh / h], score: d.score, embedding: embedder.embed(img, d)? });
            }
        }
        let clip = match &mut self.clip {
            Some(c) => Some(c.embed(p.clip)?),
            None => None,
        };
        Ok((Found { faces, clip }, p.img))
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
            let parts = Parts::find(&self.dirs);
            let Some(dir) = parts.faces.clone().or_else(|| parts.clip.clone()) else {
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
            if models.as_ref().map(|m| (m.1, m.0.bits())) != Some((want_gpu, parts.bits())) {
                models = None;
                match Models::open(&parts, want_gpu) {
                    Ok(m) => {
                        tracing::info!("recognition models loaded: {parts:?} ({:?})", m.device);
                        // Items looked at before a model was installed get its turn now.
                        match ai::requeue(lib.db(), parts.bits()) {
                            Ok(0) => {}
                            Ok(n) => tracing::info!("{n} items to look at again with the new models"),
                            Err(e) => tracing::error!("cannot queue items for the new models: {e}"),
                        }
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
            let ran = m.bits();

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
                new_faces += self.store(item, &found, ran, img.as_ref());
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
    fn store(&self, item: &Media, found: &Found, ran: i64, img: Option<&RgbImage>) -> usize {
        let lib = &self.library;
        match ai::save_parts(lib.db(), item, found, ran) {
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
