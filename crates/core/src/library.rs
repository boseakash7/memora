//! The running library: scans folders, watches them for changes, and works through
//! the processing pipeline (metadata, then thumbnails) on background threads.
//!
//! Pending work is read from the `stage` column, so it resumes after a restart.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use notify::event::{ModifyKind, RenameMode};
use notify::{EventKind, RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use r2d2_sqlite::rusqlite::params;
use serde::Serialize;

use crate::db::Db;
use crate::folders::{self, Folder};
use crate::media::{MEDIA_COLUMNS, Media, Stage};
use crate::metadata::{self, DATE_FORMAT, Metadata};
use crate::scan::{self, Change};
use crate::places::{self, PlaceName};
use crate::{AppPaths, Result, backup, hash, organize, semantic, snapshot, sync, thumbs};

/// Indexing progress, pushed to the UI.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// A folder walk is running or queued.
    pub scanning: bool,
    pub scan_folder: Option<PathBuf>,
    /// Photos and videos seen so far in the current walk.
    pub scanned_files: u64,
    /// Items still waiting for metadata or thumbnails.
    pub pending: u64,
    /// Of those, items at each step: indexing works through the whole library one step at
    /// a time (all details first, so the grid fills quickly), so `pending` only falls in
    /// the last step. Details to read, thumbnails to make, hashes and places to find.
    pub to_read: u64,
    pub to_thumbnail: u64,
    pub to_hash: u64,
    /// Items in the timeline.
    pub total: u64,
    /// Of those: photos, videos, and their size on disk in bytes.
    pub photos: u64,
    pub videos: u64,
    pub bytes: u64,
    /// Items that could not be read.
    pub errors: u64,
    /// Items waiting for face and content recognition (runs after indexing).
    pub ai_pending: u64,
    /// Goes up whenever the timeline changes, so the UI knows to reload it.
    pub revision: u64,
}

type Listener = Box<dyn Fn(&Status) + Send + Sync>;
type Watcher = Debouncer<RecommendedWatcher, RecommendedCache>;

enum Job {
    All,
    Folder(i64),
    Changes(Vec<Change>),
}

/// Handle to the running library. Cheap to clone.
#[derive(Clone)]
pub struct Library {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    paths: AppPaths,
    status: Mutex<Status>,
    last_emit: Mutex<Option<Instant>>,
    listeners: Mutex<Vec<Listener>>,
    jobs: mpsc::Sender<Job>,
    queued: AtomicU64,
    processing: AtomicBool,
    wake: (Mutex<bool>, Condvar),
    watcher: Mutex<Option<Watcher>>,
    watched: Mutex<HashSet<PathBuf>>,
    stop: AtomicBool,
    semantic: semantic::Index,
    /// Work for the library-info thread (`SYNC_*` bits), see [`crate::sync`].
    sync_work: (Mutex<u8>, Condvar),
}

const SYNC_FLUSH: u8 = 1;
const SYNC_READ: u8 = 2;
const SYNC_APPLY: u8 = 4;
/// How often index snapshots are brought up to date (see [`crate::snapshot`]).
const SNAPSHOT_SETTLED: Duration = Duration::from_secs(5 * 60);
const SNAPSHOT_BUSY: Duration = Duration::from_secs(30 * 60);

/// Pending items read from the database at a time.
const FETCH: usize = 128;
const EMIT_EVERY: Duration = Duration::from_millis(300);

impl Library {
    /// Start background threads, watch the folders and queue a full scan
    /// (which picks up anything that changed while the app was closed).
    pub fn start(db: Db, paths: AppPaths) -> Result<Library> {
        let (tx, rx) = mpsc::channel();
        let inner = Arc::new(Inner {
            db,
            paths,
            status: Mutex::new(Status::default()),
            last_emit: Mutex::new(None),
            listeners: Mutex::new(Vec::new()),
            jobs: tx,
            queued: AtomicU64::new(0),
            processing: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
            watcher: Mutex::new(None),
            watched: Mutex::new(HashSet::new()),
            stop: AtomicBool::new(false),
            semantic: semantic::Index::default(),
            sync_work: (Mutex::new(SYNC_READ | SYNC_APPLY), Condvar::new()),
        });

        let weak = Arc::downgrade(&inner);
        match new_debouncer(Duration::from_secs(2), None, move |res| on_fs_events(&weak, res)) {
            Ok(w) => *inner.watcher.lock().unwrap() = Some(w),
            Err(e) => tracing::warn!("file watching unavailable, use Rescan to pick up changes: {e}"),
        }

        let scan_inner = inner.clone();
        std::thread::Builder::new()
            .name("library-scan".into())
            .spawn(move || scan_inner.scan_loop(rx))?;
        let work_inner = inner.clone();
        std::thread::Builder::new()
            .name("library-process".into())
            .spawn(move || work_inner.process_loop())?;

        let trash_inner = inner.clone();
        std::thread::Builder::new()
            .name("library-trash".into())
            .spawn(move || trash_inner.purge_loop())?;

        let sync_inner = inner.clone();
        std::thread::Builder::new()
            .name("library-sync".into())
            .spawn(move || sync_inner.sync_loop())?;

        let dates_inner = inner.clone();
        std::thread::Builder::new()
            .name("library-video-dates".into())
            .spawn(move || dates_inner.refresh_video_dates())?;

        inner.refresh_counts();
        inner.queue(Job::All);
        Ok(Library { inner })
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn paths(&self) -> &AppPaths {
        &self.inner.paths
    }

    /// CLIP vectors for searching by what is in the pictures.
    pub fn semantic(&self) -> &semantic::Index {
        &self.inner.semantic
    }

    /// Recognition made progress: update the counts (throttled). `changed` also tells open
    /// lists to reload, e.g. when new people appeared.
    pub fn report_ai(&self, changed: bool) {
        self.inner.kick_sync(SYNC_APPLY);
        if changed {
            self.inner.status.lock().unwrap().revision += 1;
        }
        self.inner.refresh_counts();
        self.inner.emit(changed);
    }

    /// True once stopped by [`Library::shutdown`].
    pub fn is_stopped(&self) -> bool {
        self.inner.stop.load(Ordering::SeqCst)
    }

    pub fn status(&self) -> Status {
        self.inner.status.lock().unwrap().clone()
    }

    /// Called (throttled) whenever the status changes.
    pub fn subscribe(&self, f: impl Fn(&Status) + Send + Sync + 'static) {
        self.inner.listeners.lock().unwrap().push(Box::new(f));
    }

    /// Add a folder. With `keep_info`, its library info is also kept in a `.memora` folder
    /// inside it (or read from one that is there already), see [`crate::sync`].
    pub fn add_folder(&self, path: &Path, keep_info: bool) -> Result<Folder> {
        let folder = folders::add_with(&self.inner.db, path, keep_info)?;
        if keep_info {
            self.inner.kick_sync(SYNC_READ | SYNC_FLUSH);
        }
        self.inner.watch(&folder.path);
        self.inner.queue(Job::Folder(folder.id));
        Ok(folder)
    }

    /// The folder moved (or its drive letter changed): point it at `path` and look it over
    /// again. Items keep their ids, so favorites, albums, people and thumbnails stay.
    pub fn locate_folder(&self, id: i64, path: &Path) -> Result<Folder> {
        let old = folders::get(&self.inner.db, id)?;
        folders::relocate(&self.inner.db, id, path)?;
        sync::relocated(&self.inner.db, id)?;
        let folder = folders::get(&self.inner.db, id)?;
        self.inner.kick_sync(SYNC_FLUSH | SYNC_READ);
        self.inner.unwatch(&old.path);
        self.inner.watch(&folder.path);
        self.inner.queue(Job::Folder(folder.id));
        self.notify_changed();
        Ok(folder)
    }

    /// Start or stop keeping a folder's library info in `.memora`.
    pub fn set_keep_info(&self, id: i64, keep: bool) -> Result<Folder> {
        let folder = if keep { sync::enable(&self.inner.db, id)? } else { sync::disable(&self.inner.db, id)? };
        self.inner.kick_sync(SYNC_FLUSH | SYNC_READ | SYNC_APPLY);
        Ok(folder)
    }

    /// Keep the finished index in the folder's `.memora` too (on by default), or stop and
    /// delete this PC's copy there.
    pub fn set_keep_index(&self, id: i64, keep: bool) -> Result<Folder> {
        let db = &self.inner.db;
        db.conn()?.execute("UPDATE folders SET keep_index = ?2 WHERE id = ?1", r2d2_sqlite::rusqlite::params![id, keep])?;
        let folder = folders::get(db, id)?;
        if keep {
            snapshot::write_changed(db)?;
        } else {
            snapshot::remove_own(db, &folder)?;
        }
        Ok(folder)
    }

    /// Stop syncing and move the folder's `.memora` to the Recycle Bin.
    pub fn remove_folder_info(&self, id: i64) -> Result<Folder> {
        sync::remove_info(&self.inner.db, id)
    }

    /// Write everything the user made (favorites, albums, people names...) to one file.
    pub fn write_backup(&self, path: &Path) -> Result<backup::Summary> {
        backup::write(&self.inner.db, path)
    }

    /// Restore a backup file. Its folders that are not in the library but exist on this PC
    /// are added (and listed right away, so their items can be found); then everything in it
    /// is added to the library. With `with_settings`, settings are restored too.
    pub fn restore_backup(&self, path: &Path, with_settings: bool) -> Result<backup::Restored> {
        let saved = backup::read(path)?;
        let db = &self.inner.db;
        let mut added = Vec::new();
        for f in saved.folders.iter().filter(|f| f.path.is_dir()) {
            let folder = match folders::add_with(db, &f.path, f.keep_info) {
                Ok(folder) => folder,
                Err(crate::Error::FolderExists(_)) => continue,
                Err(e) => return Err(e),
            };
            if !f.keep_index {
                db.conn()?.execute("UPDATE folders SET keep_index = 0 WHERE id = ?1", [folder.id])?;
            }
            let folder = folders::get(db, folder.id)?;
            scan::scan_folder(db, &folder, &mut |_| {})?;
            if folder.sync && folder.keep_index {
                snapshot::import(db, folder.id)?;
            }
            added.push(folder);
        }
        let mut report = backup::restore(db, &saved, with_settings)?;
        report.folders_added = added.len();
        for folder in &added {
            self.inner.watch(&folder.path);
            self.inner.queue(Job::Folder(folder.id));
        }
        self.inner.semantic.reset();
        self.inner.wake();
        self.notify_changed();
        Ok(report)
    }

    /// Write pending library info and read other PCs' changes now, then apply them.
    /// Returns true if the library changed.
    pub fn sync_now(&self) -> Result<bool> {
        sync::flush(&self.inner.db)?;
        sync::read(&self.inner.db)?;
        let changed = sync::apply(&self.inner.db)?;
        snapshot::write_changed(&self.inner.db)?;
        if changed {
            self.notify_changed();
        }
        Ok(changed)
    }

    /// Forget a folder and everything indexed in it. Files on disk are not touched.
    pub fn remove_folder(&self, id: i64) -> Result<()> {
        let folder = folders::get(&self.inner.db, id)?;
        self.inner.unwatch(&folder.path);
        let ids: Vec<i64> = {
            let conn = self.inner.db.conn()?;
            let mut stmt = conn.prepare("SELECT id FROM media WHERE folder_id = ?1")?;
            stmt.query_map([id], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?
        };
        folders::remove(&self.inner.db, id)?;
        thumbs::remove_cached(&self.inner.paths, &ids);
        self.inner.status.lock().unwrap().revision += 1;
        self.inner.refresh_counts();
        self.inner.emit(true);
        Ok(())
    }

    /// Tell the UI that items changed (favorites, trash, albums...) so it reloads its lists.
    pub fn notify_changed(&self) {
        self.inner.kick_sync(SYNC_FLUSH);
        self.inner.status.lock().unwrap().revision += 1;
        self.inner.refresh_counts();
        self.inner.emit(true);
    }

    /// Walk every folder again.
    pub fn rescan(&self) {
        self.inner.queue(Job::All);
    }

    /// Stop background work. Used by tests; the app just exits.
    pub fn shutdown(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        if let Some(w) = self.inner.watcher.lock().unwrap().take() {
            w.stop_nonblocking();
        }
        self.inner.wake();
        self.inner.kick_sync(0);
        // Unblock the scan thread's recv().
        let _ = self.inner.jobs.send(Job::Changes(Vec::new()));
    }

    /// Block until all queued scans and pending items are done. For tests.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            let s = self.status();
            if self.inner.queued.load(Ordering::SeqCst) == 0
                && !self.inner.processing.load(Ordering::SeqCst)
                && !s.scanning
                && s.pending == 0
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Inner {
    fn queue(&self, job: Job) {
        if self.queued.fetch_add(1, Ordering::SeqCst) == 0 {
            self.status.lock().unwrap().scanning = true;
            self.emit(true);
        }
        if self.jobs.send(job).is_err() {
            self.queued.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn emit(&self, force: bool) {
        {
            let mut last = self.last_emit.lock().unwrap();
            if !force && last.is_some_and(|t| t.elapsed() < EMIT_EVERY) {
                return;
            }
            *last = Some(Instant::now());
        }
        let status = self.status.lock().unwrap().clone();
        for listener in self.listeners.lock().unwrap().iter() {
            listener(&status);
        }
    }

    fn kick_sync(&self, work: u8) {
        let (lock, cvar) = &self.sync_work;
        *lock.lock().unwrap() |= work;
        cvar.notify_one();
    }

    /// Keep `.memora` folders and the library in step: write this PC's changes soon after
    /// they are made, read other PCs' changes when their logs change (the folder watcher
    /// sees them) or every half minute (network drives may not report changes).
    fn sync_loop(&self) {
        const POLL: Duration = Duration::from_secs(30);
        if let Err(e) = sync::compact(&self.db) {
            tracing::warn!("cannot compact library info: {e}");
        }
        let mut last_read = Instant::now();
        // The first look at index snapshots comes a minute after starting.
        let mut last_snapshot = Instant::now().checked_sub(SNAPSHOT_SETTLED - Duration::from_secs(60)).unwrap_or_else(Instant::now);
        while !self.stop.load(Ordering::SeqCst) {
            let work = {
                let (lock, cvar) = &self.sync_work;
                let mut work = lock.lock().unwrap();
                if *work == 0 {
                    work = cvar.wait_timeout(work, Duration::from_secs(2)).unwrap().0;
                }
                std::mem::take(&mut *work)
            };
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let mut run = || -> Result<bool> {
                sync::flush(&self.db)?;
                let mut apply = work & SYNC_APPLY != 0;
                if work & SYNC_READ != 0 || last_read.elapsed() >= POLL {
                    last_read = Instant::now();
                    apply |= sync::read(&self.db)?;
                }
                let changed = apply && sync::apply(&self.db)?;
                // Index snapshots: soon after indexing and recognition settle, and now and
                // then while recognition is still going (it can take hours).
                let every = if self.status.lock().unwrap().ai_pending == 0 { SNAPSHOT_SETTLED } else { SNAPSHOT_BUSY };
                if last_snapshot.elapsed() >= every {
                    last_snapshot = Instant::now();
                    snapshot::write_changed(&self.db)?;
                }
                Ok(changed)
            };
            match run() {
                Ok(true) => {
                    self.status.lock().unwrap().revision += 1;
                    self.refresh_counts();
                    self.emit(true);
                }
                Ok(false) => {}
                Err(e) => tracing::warn!("library info sync: {e}"),
            }
        }
    }

    fn wake(&self) {
        let (lock, cvar) = &self.wake;
        *lock.lock().unwrap() = true;
        cvar.notify_one();
    }

    fn refresh_counts(&self) {
        let counts = self.db.conn().and_then(|c| {
            Ok(c.query_row(
                "SELECT count(*) FILTER (WHERE stage >= ?1 AND trashed_at IS NULL AND archived_at IS NULL),
                        count(*) FILTER (WHERE stage < ?2),
                        count(*) FILTER (WHERE error IS NOT NULL),
                        count(*) FILTER (WHERE stage = ?3),
                        count(*) FILTER (WHERE stage >= ?1 AND trashed_at IS NULL AND archived_at IS NULL AND kind = 'photo'),
                        count(*) FILTER (WHERE stage >= ?1 AND trashed_at IS NULL AND archived_at IS NULL AND kind = 'video'),
                        coalesce(sum(size) FILTER (WHERE stage >= ?1 AND trashed_at IS NULL AND archived_at IS NULL), 0),
                        count(*) FILTER (WHERE stage = ?4),
                        count(*) FILTER (WHERE stage = ?1),
                        count(*) FILTER (WHERE stage = ?5)
                 FROM media WHERE missing = 0",
                [Stage::METADATA, Stage::DONE, Stage::HASH, Stage::NEW, Stage::THUMBNAIL],
                |r| {
                    let n = |i| r.get::<_, i64>(i).map(|n| n as u64);
                    Ok(([n(0)?, n(1)?, n(2)?, n(3)?], [n(4)?, n(5)?, n(6)?], [n(7)?, n(8)?, n(9)?]))
                },
            )?)
        });
        match counts {
            Ok(([total, pending, errors, ai_pending], [photos, videos, bytes], [to_read, to_thumbnail, to_hash])) => {
                let mut s = self.status.lock().unwrap();
                s.to_read = to_read;
                s.to_thumbnail = to_thumbnail;
                s.to_hash = to_hash;
                s.total = total;
                s.photos = photos;
                s.videos = videos;
                s.bytes = bytes;
                s.pending = pending;
                s.errors = errors;
                s.ai_pending = ai_pending;
            }
            Err(e) => tracing::error!("cannot count library items: {e}"),
        }
    }

    fn watch(&self, path: &Path) {
        let mut watched = self.watched.lock().unwrap();
        if watched.contains(path) {
            return;
        }
        if let Some(w) = self.watcher.lock().unwrap().as_mut() {
            match w.watch(path, RecursiveMode::Recursive) {
                Ok(()) => {
                    watched.insert(path.to_path_buf());
                }
                Err(e) => tracing::warn!("cannot watch {}: {e}", path.display()),
            }
        }
    }

    fn unwatch(&self, path: &Path) {
        if self.watched.lock().unwrap().remove(path)
            && let Some(w) = self.watcher.lock().unwrap().as_mut()
        {
            let _ = w.unwatch(path);
        }
    }

    // ---------- trash ----------

    /// Remove items that have been in the trash for 30 days: at startup, then every few hours.
    fn purge_loop(&self) {
        const EVERY: Duration = Duration::from_secs(6 * 3600);
        let mut next = Instant::now();
        while !self.stop.load(Ordering::SeqCst) {
            if Instant::now() >= next {
                next = Instant::now() + EVERY;
                match organize::purge_expired(&self.db, &self.paths) {
                    Ok(r) => {
                        if r.deleted > 0 {
                            tracing::info!("moved {} expired trash items to the Recycle Bin", r.deleted);
                            self.status.lock().unwrap().revision += 1;
                            self.refresh_counts();
                            self.emit(true);
                        }
                        r.failed.iter().for_each(|f| tracing::warn!("cannot empty trash: {f}"));
                    }
                    Err(e) => tracing::error!("cannot purge the trash: {e}"),
                }
            }
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    // ---------- one-off upgrades ----------

    /// Read the dates of already indexed videos again with the current rules, once (the
    /// flag is set by migration 8). Only the date and offset are updated.
    fn refresh_video_dates(&self) {
        const FLAG: &str = "_refreshVideoDates";
        let run = || -> Result<usize> {
            let flagged: bool = self.db.conn()?.query_row("SELECT count(*) FROM settings WHERE key = ?1", [FLAG], |r| r.get(0))?;
            if !flagged {
                return Ok(0);
            }
            let videos: Vec<(i64, String, i64)> = {
                let conn = self.db.conn()?;
                let mut stmt = conn.prepare(
                    "SELECT id, path, mtime FROM media
                     WHERE kind = 'video' AND missing = 0 AND stage >= ?1 AND error IS NULL",
                )?;
                stmt.query_map([Stage::METADATA], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<std::result::Result<_, _>>()?
            };
            let mut changed = 0;
            for (id, path, mtime) in videos {
                if self.stop.load(Ordering::SeqCst) {
                    return Ok(changed); // flag stays, so this resumes next time
                }
                let Ok(meta) = metadata::read(Path::new(&path), crate::media::Format::Video) else { continue };
                let Some(taken) = meta.taken_at else { continue };
                changed += self.db.conn()?.execute(
                    "UPDATE media SET taken_at = ?2, tz_offset = ?3
                     WHERE id = ?1 AND mtime = ?4 AND (taken_at IS NOT ?2 OR tz_offset IS NOT ?3)",
                    params![id, taken.format(DATE_FORMAT).to_string(), meta.tz_offset, mtime],
                )?;
            }
            self.db.conn()?.execute("DELETE FROM settings WHERE key = ?1", [FLAG])?;
            Ok(changed)
        };
        match run() {
            Ok(0) => {}
            Ok(n) => {
                tracing::info!("corrected the dates of {n} videos");
                self.status.lock().unwrap().revision += 1;
                self.emit(true);
            }
            Err(e) => tracing::warn!("cannot re-read video dates: {e}"),
        }
    }

    // ---------- scanning ----------

    fn scan_loop(&self, rx: mpsc::Receiver<Job>) {
        while let Ok(job) = rx.recv() {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            if let Err(e) = self.run_job(job) {
                tracing::error!("library scan failed: {e}");
            }
            // Counts first: the moment scanning ends, `pending` must already include what
            // the scan found, or the library looks idle with work still to do.
            self.refresh_counts();
            if self.queued.fetch_sub(1, Ordering::SeqCst) == 1 {
                let mut s = self.status.lock().unwrap();
                s.scanning = false;
                s.scan_folder = None;
                s.scanned_files = 0;
            }
            self.wake();
            // Newly found items may be the ones other PCs' changes are about.
            self.kick_sync(SYNC_APPLY);
            self.emit(true);
        }
    }

    fn run_job(&self, job: Job) -> Result<()> {
        let changed = match job {
            Job::All => {
                let mut changed = false;
                for folder in folders::list(&self.db)?.into_iter().filter(|f| f.enabled) {
                    changed |= self.scan_one(&folder)?;
                }
                changed
            }
            Job::Folder(id) => match folders::get(&self.db, id) {
                Ok(folder) => self.scan_one(&folder)?,
                Err(_) => false, // removed while queued
            },
            Job::Changes(changes) if changes.is_empty() => false,
            Job::Changes(changes) => {
                let all = folders::list(&self.db)?;
                scan::apply_changes(&self.db, &all, &changes)?
            }
        };
        if changed {
            self.status.lock().unwrap().revision += 1;
        }
        Ok(())
    }

    fn scan_one(&self, folder: &Folder) -> Result<bool> {
        if !folder.available {
            // Unplugged drive: keep what we know rather than marking it all missing.
            tracing::info!("skipping unavailable folder {}", folder.path.display());
            return Ok(false);
        }
        self.watch(&folder.path);
        {
            let mut s = self.status.lock().unwrap();
            s.scan_folder = Some(folder.path.clone());
            s.scanned_files = 0;
        }
        self.emit(true);
        let started = Instant::now();
        let stats = scan::scan_folder(&self.db, folder, &mut |n| {
            self.status.lock().unwrap().scanned_files = n;
            self.emit(false);
        })?;
        tracing::info!("scanned {} in {:.1?}: {stats:?}", folder.path.display(), started.elapsed());
        if stats.added + stats.changed > 0 && folder.sync && folder.keep_index {
            // Other PCs may have indexed these files already (see `snapshot`).
            match snapshot::import(&self.db, folder.id) {
                Ok(n) if n.recognised > 0 => self.semantic.reset(),
                Ok(_) => {}
                Err(e) => tracing::warn!("cannot use the index in {}: {e}", folder.path.display()),
            }
        }
        if stats.added + stats.changed > 0 {
            self.wake();
        }
        Ok(stats.any_change())
    }

    // ---------- processing ----------

    /// Feed pending items to worker threads and store what they find.
    ///
    /// Workers take items one at a time from a short queue, so a slow file (a long video, a
    /// big HEIC) holds up only its own worker. They run at below-normal priority, as do the
    /// FFmpeg processes they start, so indexing can use most of the CPU without making the
    /// PC sluggish.
    fn process_loop(&self) {
        let threads = worker_count();
        let (job_tx, job_rx) = mpsc::sync_channel::<Media>(threads * 2);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (done_tx, done_rx) = mpsc::channel::<(Media, Outcome)>();
        for i in 0..threads {
            let (jobs, done, paths) = (job_rx.clone(), done_tx.clone(), self.paths.clone());
            let spawned = std::thread::Builder::new().name(format!("library-worker-{i}")).spawn(move || {
                crate::ffmpeg::mark_background_thread();
                loop {
                    let next = jobs.lock().unwrap().recv();
                    let Ok(m) = next else { break };
                    let outcome = process(&paths, &m);
                    if done.send((m, outcome)).is_err() {
                        break;
                    }
                }
            });
            if let Err(e) = spawned {
                tracing::error!("cannot start a library worker: {e}");
            }
        }
        drop(done_tx);

        let mut queue: std::collections::VecDeque<Media> = Default::default();
        let mut in_flight: HashSet<i64> = HashSet::new();
        let mut last_counts = Instant::now();
        while !self.stop.load(Ordering::SeqCst) {
            // Keep every worker busy with one item running and one waiting.
            while in_flight.len() < threads * 2 {
                if queue.is_empty() {
                    match self.next_items(in_flight.len() + FETCH) {
                        Ok(items) => queue.extend(items.into_iter().filter(|m| !in_flight.contains(&m.id))),
                        Err(e) => tracing::error!("cannot read pending items: {e}"),
                    }
                }
                let Some(m) = queue.pop_front() else { break };
                in_flight.insert(m.id);
                if job_tx.send(m).is_err() {
                    return;
                }
            }

            if in_flight.is_empty() {
                if self.processing.swap(false, Ordering::SeqCst) {
                    // Hashes let other PCs' changes find renamed files.
                    self.kick_sync(SYNC_APPLY);
                    // Places and hashes arrived while working; let open lists pick them up.
                    self.status.lock().unwrap().revision += 1;
                    self.refresh_counts();
                    self.emit(true);
                }
                let (lock, cvar) = &self.wake;
                let mut woken = lock.lock().unwrap();
                if !*woken {
                    woken = cvar.wait_timeout(woken, Duration::from_secs(60)).unwrap().0;
                }
                *woken = false;
                continue;
            }

            self.processing.store(true, Ordering::SeqCst);
            let mut results = Vec::new();
            if let Ok(first) = done_rx.recv_timeout(Duration::from_millis(500)) {
                results.push(first);
                results.extend(done_rx.try_iter());
            }
            if results.is_empty() {
                continue;
            }
            for (m, _) in &results {
                in_flight.remove(&m.id);
            }
            let refs: Vec<(&Media, &Outcome)> = results.iter().map(|(m, o)| (m, o)).collect();
            match self.save(&refs) {
                Ok(visible) => {
                    if visible {
                        self.status.lock().unwrap().revision += 1;
                    }
                }
                Err(e) => {
                    tracing::error!("cannot save processing results: {e}");
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            // Counting a big library takes a moment, so not after every item.
            if last_counts.elapsed() >= EMIT_EVERY {
                last_counts = Instant::now();
                self.refresh_counts();
                self.emit(false);
            }
        }
    }

    /// Next items to work on: all metadata first (so the grid fills quickly), newest first.
    fn next_items(&self, limit: usize) -> Result<Vec<Media>> {
        let conn = self.db.conn()?;
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT {MEDIA_COLUMNS} FROM media WHERE missing = 0 AND stage < ?1
             ORDER BY stage, taken_at DESC LIMIT ?2"
        ))?;
        let items = stmt.query_map(params![Stage::DONE, limit as i64], Media::from_row)?;
        Ok(items.collect::<std::result::Result<_, _>>()?)
    }

    /// Store results. Returns true if items became visible in the timeline.
    fn save(&self, results: &[(&Media, &Outcome)]) -> Result<bool> {
        let mut conn = self.db.conn()?;
        let tx = conn.transaction()?;
        let mut visible = false;
        for (m, outcome) in results {
            // The guard on stage+mtime skips items that changed on disk meanwhile.
            let n = match outcome {
                Outcome::Metadata(Ok(meta)) => {
                    let taken_at = meta.taken_at.unwrap_or_else(|| metadata::local_time_from_millis(m.mtime));
                    tx.prepare_cached(
                        "UPDATE media SET width = ?3, height = ?4, duration_ms = ?5, orientation = ?6,
                             taken_at = ?7, tz_offset = ?8, camera_make = ?9, camera_model = ?10,
                             lat = ?11, lon = ?12, stage = ?13, error = NULL,
                             video_codec = ?15, audio_codec = ?16
                         WHERE id = ?1 AND stage = ?2 AND mtime = ?14",
                    )?
                    .execute(params![
                        m.id,
                        m.stage,
                        meta.width,
                        meta.height,
                        meta.duration_ms,
                        meta.orientation,
                        taken_at.format(DATE_FORMAT).to_string(),
                        meta.tz_offset,
                        meta.camera_make,
                        meta.camera_model,
                        meta.lat,
                        meta.lon,
                        Stage::METADATA,
                        m.mtime,
                        meta.video_codec,
                        meta.audio_codec,
                    ])?
                }
                Outcome::Analysis(Ok(a)) => {
                    let place_id = match &a.place {
                        Some(place) => Some(places::intern(&tx, place)?),
                        None => None,
                    };
                    tx.prepare_cached(
                        "UPDATE media SET blake3 = ?3, phash = ?4, place_id = ?5, stage = ?6
                         WHERE id = ?1 AND stage = ?2 AND mtime = ?7",
                    )?
                    .execute(params![m.id, m.stage, a.blake3, a.phash, place_id, Stage::HASH, m.mtime])?
                }
                Outcome::Metadata(Err(e)) | Outcome::Thumbnail(Err(e)) | Outcome::Analysis(Err(e)) => {
                    tracing::debug!("{}: {e}", m.path.display());
                    tx.prepare_cached(
                        "UPDATE media SET stage = stage + 1, error = ?3 WHERE id = ?1 AND stage = ?2 AND mtime = ?4",
                    )?
                    .execute(params![m.id, m.stage, e.to_string(), m.mtime])?
                }
                // Items whose hashes (and maybe faces) came from an index snapshot are done.
                Outcome::Thumbnail(Ok(())) => tx
                    .prepare_cached(
                        "UPDATE media SET stage = CASE imported WHEN 2 THEN ?5 WHEN 1 THEN ?6 ELSE ?3 END
                         WHERE id = ?1 AND stage = ?2 AND mtime = ?4",
                    )?
                    .execute(params![m.id, m.stage, Stage::THUMBNAIL, m.mtime, Stage::AI, Stage::HASH])?,
            };
            visible |= n > 0 && m.stage == Stage::NEW;
        }
        tx.commit()?;
        Ok(visible)
    }
}

enum Outcome {
    Metadata(Result<Metadata>),
    Thumbnail(Result<()>),
    Analysis(Result<Analysis>),
}

/// What the hash stage learns about a file.
struct Analysis {
    blake3: String,
    /// Perceptual hash; None for pictures without detail or that cannot be decoded.
    phash: Option<i64>,
    place: Option<PlaceName>,
}

fn analyze(paths: &AppPaths, m: &Media) -> Result<Analysis> {
    let blake3 = hash::file_blake3(&m.path)?;
    // A thumbnail that cannot be made (broken file) just means no perceptual hash.
    let phash = thumbs::ensure_thumb(paths, m)
        .ok()
        .and_then(|thumb| hash::phash_file(&thumb).ok().flatten())
        .map(|h| h as i64);
    let place = match (m.lat, m.lon) {
        (Some(lat), Some(lon)) => places::lookup(lat, lon),
        _ => None,
    };
    Ok(Analysis { blake3, phash, place })
}

/// Indexing threads: all logical cores but two (left for the app and the rest of Windows),
/// at most 8. More gain little (HEIC decoding tops out around 6-8 FFmpeg processes on an
/// 8-core PC) and cost memory: FFmpeg needs ~200 MB to open a tiled phone photo.
fn worker_count() -> usize {
    std::thread::available_parallelism().map(|n| n.get().saturating_sub(2)).unwrap_or(2).clamp(1, 8)
}

fn process(paths: &AppPaths, m: &Media) -> Outcome {
    if m.stage == Stage::NEW {
        // New or changed on disk: anything cached for it is out of date.
        thumbs::remove_cached(paths, &[m.id]);
        Outcome::Metadata(metadata::read(&m.path, m.format()))
    } else if m.stage == Stage::METADATA {
        Outcome::Thumbnail(thumbs::ensure_thumb(paths, m).map(|_| ()))
    } else {
        Outcome::Analysis(analyze(paths, m))
    }
}

// ---------- file watching ----------

fn on_fs_events(inner: &Weak<Inner>, res: DebounceEventResult) {
    let Some(inner) = inner.upgrade() else { return };
    match res {
        Ok(events) => {
            let mut changes: Vec<Change> = Vec::new();
            let mut rescan = false;
            for ev in events {
                if ev.need_rescan() {
                    rescan = true;
                    continue;
                }
                if ev.paths.iter().any(|p| sync::is_info_path(p)) {
                    inner.kick_sync(SYNC_READ);
                    continue;
                }
                let paths = &ev.paths;
                let mut push = |c: Change| {
                    if changes.last() != Some(&c) {
                        changes.push(c);
                    }
                };
                match ev.kind {
                    EventKind::Access(_) => {}
                    EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if paths.len() == 2 => {
                        push(Change::Renamed(paths[0].clone(), paths[1].clone()))
                    }
                    EventKind::Modify(ModifyKind::Name(RenameMode::From)) | EventKind::Remove(_) => {
                        paths.iter().for_each(|p| push(Change::Removed(p.clone())))
                    }
                    EventKind::Create(_) | EventKind::Modify(ModifyKind::Name(_)) => {
                        paths.iter().for_each(|p| push(Change::Created(p.clone())))
                    }
                    _ => paths.iter().for_each(|p| push(Change::Modified(p.clone()))),
                }
            }
            if rescan {
                inner.queue(Job::All);
            } else if !changes.is_empty() {
                inner.queue(Job::Changes(changes));
            }
        }
        Err(errors) => {
            for e in errors {
                tracing::warn!("file watcher error: {e}");
            }
            inner.queue(Job::All);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media;

    fn start(dir: &Path) -> Library {
        let paths = AppPaths::in_dir(&dir.join("data")).unwrap();
        let db = Db::open(&paths.db_path).unwrap();
        Library::start(db, paths).unwrap()
    }

    #[test]
    fn indexes_new_folder_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Photos");
        std::fs::create_dir_all(&root).unwrap();
        image::RgbImage::from_pixel(800, 600, image::Rgb([200, 30, 30])).save(root.join("red.jpg")).unwrap();
        image::RgbaImage::new(50, 100).save(root.join("tall.png")).unwrap();
        std::fs::write(root.join("broken.jpg"), b"not a jpeg").unwrap();
        // A camera RAW file whose 1200x800 preview is shown turned (portrait).
        crate::raw::test_files::write_raw(&root.join("camera.cr2"), 1200, 800);
        let has_ffmpeg = crate::ffmpeg::available();
        if has_ffmpeg {
            crate::ffmpeg::make_test_video(&root.join("clip.mp4"), &[]);
        }

        let lib = start(dir.path());
        lib.add_folder(&root, false).unwrap();
        assert!(lib.wait_idle(Duration::from_secs(60)), "indexing finished");

        let items = media::timeline(lib.db()).unwrap();
        let expected = if has_ffmpeg { 5 } else { 4 };
        assert_eq!(items.len(), expected);
        let s = lib.status();
        assert_eq!((s.total, s.pending, s.errors), (expected as u64, 0, 1));
        assert_eq!((s.photos, s.videos), (4, has_ffmpeg as u64), "the unreadable JPEG still counts as a photo");
        let on_disk: u64 = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum();
        assert_eq!(s.bytes, on_disk);
        assert!(s.revision > 0);

        for item in &items {
            let m = media::get(lib.db(), item.id).unwrap();
            match m.name.as_str() {
                "red.jpg" => assert_eq!((m.width, m.height), (Some(800), Some(600))),
                "tall.png" => assert_eq!((m.width, m.height), (Some(50), Some(100))),
                "camera.cr2" => {
                    assert_eq!((m.width, m.height, m.orientation), (Some(800), Some(1200), 6));
                    assert_eq!(m.camera_make.as_deref(), Some("Canon"));
                    // The viewer gets an upright JPEG copy.
                    let display = image::open(thumbs::ensure_display(lib.paths(), &m).unwrap()).unwrap();
                    assert_eq!((display.width(), display.height()), (800, 1200));
                }
                "clip.mp4" => {
                    assert_eq!((m.width, m.height), (Some(320), Some(240)));
                    assert_eq!(m.duration_ms, Some(2000));
                }
                "broken.jpg" => {
                    assert!(m.error.is_some());
                    continue;
                }
                other => panic!("unexpected {other}"),
            }
            let thumb = image::open(thumbs::thumb_path(lib.paths(), m.id)).unwrap();
            assert_eq!(thumb.height(), m.height.unwrap().min(thumbs::THUMB_HEIGHT));
        }

        // Removing the folder forgets its items and their cached files.
        let red = items.iter().find(|i| i.w == 800).unwrap().id;
        let folder_id = folders::list(lib.db()).unwrap()[0].id;
        lib.remove_folder(folder_id).unwrap();
        assert!(media::timeline(lib.db()).unwrap().is_empty());
        assert!(!thumbs::thumb_path(lib.paths(), red).exists());
        lib.shutdown();
    }

    #[test]
    fn hashes_places_and_duplicates_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Photos");
        std::fs::create_dir_all(&root).unwrap();

        // A picture with real detail, a byte-for-byte copy, and a half-size re-save.
        let scene = image::RgbImage::from_fn(640, 480, |x, y| {
            let (fx, fy) = (x as f32 / 640.0, y as f32 / 480.0);
            let blob = if (fx - 0.3).powi(2) + (fy - 0.6).powi(2) < 0.05 { 255.0 } else { 0.0 };
            image::Rgb([
                ((fx * 20.0).sin() * 127.0 + 128.0) as u8,
                ((fy * 13.0).cos() * 127.0 + 128.0) as u8,
                blob as u8,
            ])
        });
        scene.save(root.join("scene.jpg")).unwrap();
        std::fs::copy(root.join("scene.jpg"), root.join("scene copy.jpg")).unwrap();
        image::DynamicImage::ImageRgb8(scene)
            .resize_exact(320, 240, image::imageops::FilterType::Lanczos3)
            .save(root.join("scene small.jpg"))
            .unwrap();
        image::RgbImage::from_fn(300, 300, |x, y| image::Rgb([(x ^ y) as u8, (x * 3) as u8, (y * 7) as u8]))
            .save(root.join("different.jpg"))
            .unwrap();
        // Camera photo taken in Bengaluru (see metadata::test_jpeg_with_exif).
        metadata::test_jpeg_with_exif(&root.join("bengaluru.jpg"), 80, 60);

        let lib = start(dir.path());
        lib.add_folder(&root, false).unwrap();
        assert!(lib.wait_idle(Duration::from_secs(60)));

        // Every item went through the hash stage.
        let unhashed: i64 = lib
            .db()
            .conn()
            .unwrap()
            .query_row("SELECT count(*) FROM media WHERE blake3 IS NULL OR stage < ?1", [Stage::HASH], |r| r.get(0))
            .unwrap();
        assert_eq!(unhashed, 0);

        let groups = crate::duplicates::find(lib.db()).unwrap();
        assert_eq!(groups.len(), 1, "{groups:?}");
        let names: Vec<_> = groups[0].items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names.len(), 3);
        assert_eq!(groups[0].items[0].w, 640, "the full-size picture is the keeper");
        assert!(!names.contains(&"different.jpg"));

        // The photo with GPS got a place, searchable by city and by map area.
        let places = places::list(lib.db()).unwrap();
        assert_eq!(places.len(), 1, "{places:?}");
        assert_eq!((places[0].city.as_str(), places[0].country.as_str(), places[0].count), ("Bangalore", "IN", 1));
        let by = |q: media::Query| media::query(lib.db(), &q).unwrap().len();
        assert_eq!(by(media::Query { place: Some(places[0].id), ..Default::default() }), 1);
        assert_eq!(by(media::Query { q: Some("bangal".into()), ..Default::default() }), 1);
        assert_eq!(by(media::Query { bbox: Some("77,12,78,13".into()), ..Default::default() }), 1);
        assert_eq!(by(media::Query { bbox: Some("0,0,10,10".into()), ..Default::default() }), 0);
        assert_eq!(media::map_points(lib.db()).unwrap().len(), 1);

        // The info panel gets the place and exposure settings; a plain photo gets neither.
        let id_of_width = |w| media::timeline(lib.db()).unwrap().into_iter().find(|i| i.w == w).unwrap().id;
        // bengaluru.jpg is 80x60 but rotated, so it is shown 60 wide.
        let info = media::info(lib.db(), id_of_width(60)).unwrap();
        assert_eq!(info.place.map(|p| p.city), Some("Bangalore".to_owned()));
        assert_eq!((info.details.iso, info.details.exposure.as_deref()), (Some(400), Some("1/250")));
        let plain = media::info(lib.db(), id_of_width(300)).unwrap();
        assert_eq!((plain.place, plain.details), (None, metadata::Details::default()));
        assert!(matches!(media::info(lib.db(), 9999), Err(crate::Error::NotFound)));
        lib.shutdown();
    }

    /// Time the indexing stages on a real folder (files are only read):
    /// `PHOTOS_BENCH_DIR=folder cargo test -p photos-core --release -- --ignored --nocapture bench_indexing`
    #[test]
    #[ignore]
    fn bench_indexing() {
        let root = PathBuf::from(std::env::var("PHOTOS_BENCH_DIR").expect("set PHOTOS_BENCH_DIR"));
        let dir = tempfile::tempdir().unwrap();
        let lib = start(dir.path());
        let started = Instant::now();
        lib.add_folder(&root, false).unwrap();
        let count_below = |stage: i64| -> i64 {
            lib.db()
                .conn()
                .unwrap()
                .query_row("SELECT count(*) FROM media WHERE stage < ?1", [stage], |r| r.get(0))
                .unwrap()
        };
        let mut reported = Stage::NEW;
        let total = loop {
            std::thread::sleep(Duration::from_millis(50));
            let s = lib.status();
            if s.scanning || s.total + s.pending == 0 {
                continue;
            }
            for stage in [Stage::METADATA, Stage::THUMBNAIL, Stage::HASH] {
                if stage > reported && count_below(stage) == 0 {
                    reported = stage;
                    let name = ["", "metadata (grid filled)", "thumbnails", "hashes"][stage as usize];
                    println!("{name}: {:.1?}", started.elapsed());
                }
            }
            if reported == Stage::HASH {
                break s.total;
            }
        };
        let secs = started.elapsed().as_secs_f64();
        println!("{total} items in {secs:.1} s = {:.1} items/s", total as f64 / secs);
        lib.shutdown();
    }

    #[test]
    fn watcher_picks_up_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Photos");
        std::fs::create_dir_all(&root).unwrap();
        let lib = start(dir.path());
        let folder = lib.add_folder(&root, false).unwrap();
        assert!(lib.wait_idle(Duration::from_secs(20)));

        image::RgbImage::new(10, 10).save(folder.path.join("new.png")).unwrap();
        let start = Instant::now();
        while media::timeline(lib.db()).unwrap().is_empty() {
            assert!(start.elapsed() < Duration::from_secs(20), "watcher saw the new file");
            std::thread::sleep(Duration::from_millis(100));
        }
        lib.shutdown();
    }
}
