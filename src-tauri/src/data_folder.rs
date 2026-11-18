//! Settings → Storage: where the library data lives, and moving it (see `photos_core::storage`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use photos_core::{AppPaths, Library, storage};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::desktop;

/// Room left on the drive besides the copy, so the PC isn't left with a full disk.
const SPARE: u64 = 500 * 1024 * 1024;

static MOVING: AtomicBool = AtomicBool::new(false);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataFolder {
    data_dir: PathBuf,
    /// Database and cache, in bytes.
    size: u64,
    /// False when the data is in Memora's usual folder.
    moved: bool,
}

#[tauri::command]
pub async fn data_folder(paths: State<'_, AppPaths>) -> Result<DataFolder, String> {
    let paths = paths.inner().clone();
    tauri::async_runtime::spawn_blocking(move || DataFolder {
        size: storage::data_size(&paths),
        moved: paths.data_dir != paths.home_dir,
        data_dir: paths.data_dir,
    })
    .await
    .map_err(|e| e.to_string())
}

/// Where a move to the folder the user picked would go, or why it can't.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Target {
    target: PathBuf,
    size: u64,
    free: u64,
}

fn check(paths: &AppPaths, picked: &Path) -> Result<Target, String> {
    let target = storage::target_for(paths, picked).map_err(|e| e.to_string())?;
    let free = desktop::free_space(&target)?;
    let size = storage::data_size(paths);
    if free < size + SPARE {
        return Err(format!(
            "there isn't enough free space there: the library data needs {} MB and {} MB are free",
            (size + SPARE) / 1_000_000,
            free / 1_000_000
        ));
    }
    Ok(Target { target, size, free })
}

#[tauri::command]
pub async fn data_folder_target(path: PathBuf, paths: State<'_, AppPaths>) -> Result<Target, String> {
    let paths = paths.inner().clone();
    tauri::async_runtime::spawn_blocking(move || check(&paths, &path)).await.map_err(|e| e.to_string())?
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    /// `copy` while the app keeps working, then `finish` (background work stopped).
    phase: &'static str,
    done: u64,
    total: u64,
}

/// Why a move failed, and whether the app must restart (background work was already stopped).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveError {
    message: String,
    restart: bool,
}

/// Copy the library data to the folder the user picked, switch to it and restart.
/// Never returns on success.
#[tauri::command]
pub async fn move_data_folder(app: AppHandle, path: PathBuf) -> Result<(), MoveError> {
    let fail = |message: String, restart: bool| MoveError { message, restart };
    if MOVING.swap(true, Ordering::SeqCst) {
        return Err(fail("the library data is already being moved".into(), false));
    }
    let result = run_move(&app, path).await;
    MOVING.store(false, Ordering::SeqCst);
    result?;
    tracing::info!("library data moved; restarting");
    app.restart()
}

async fn run_move(app: &AppHandle, picked: PathBuf) -> Result<(), MoveError> {
    let paths = app.state::<AppPaths>().inner().clone();
    let library = app.state::<Library>().inner().clone();
    let server = app.state::<Arc<photos_server::LocalServer>>().inner().clone();

    let (p, picked2) = (paths.clone(), picked.clone());
    let target = tauri::async_runtime::spawn_blocking(move || check(&p, &picked2))
        .await
        .map_err(|e| MoveError { message: e.to_string(), restart: false })?
        .map_err(|message| MoveError { message, restart: false })?;
    tracing::info!("moving the library data from {} to {}", paths.data_dir.display(), target.target.display());

    // 1. Copy the cache while the app keeps working.
    let (p, t, a) = (paths.clone(), target.target.clone(), app.clone());
    let total = target.size;
    let copied = tauri::async_runtime::spawn_blocking(move || {
        let mut done = 0;
        let mut last = Instant::now();
        let r = storage::copy_cache(&p, &t, &mut |n| {
            done += n;
            if last.elapsed() > Duration::from_millis(100) {
                last = Instant::now();
                let _ = a.emit("data-move", Progress { phase: "copy", done, total });
            }
        });
        r.map(|()| done)
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r.map_err(|e| e.to_string()));
    let copied = match copied {
        Ok(n) => n,
        Err(message) => {
            storage::abandon(&target.target, &paths);
            return Err(MoveError { message, restart: false });
        }
    };

    // 2. Stop background work (indexing, recognition, TV sharing), then take a consistent
    //    copy of the database and the last cache changes.
    let _ = app.emit("data-move", Progress { phase: "finish", done: copied, total });
    library.shutdown();
    server.shutdown().await;
    tokio_sleep(Duration::from_millis(500)).await;
    let (p, t) = (paths.clone(), target.target.clone());
    let finished = tauri::async_runtime::spawn_blocking(move || {
        let r = storage::finish_copy(library.db(), &p, &t, &mut |_| {});
        if r.is_err() {
            storage::abandon(&t, &p);
        }
        r
    })
    .await;
    match finished {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(MoveError { message: e.to_string(), restart: true }),
        Err(e) => Err(MoveError { message: e.to_string(), restart: true }),
    }
}

async fn tokio_sleep(d: Duration) {
    let _ = tauri::async_runtime::spawn_blocking(move || std::thread::sleep(d)).await;
}

#[tauri::command]
pub fn restart_app(app: AppHandle) {
    app.restart()
}
