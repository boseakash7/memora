mod data_folder;
mod desktop;
mod window_chrome;

use std::path::PathBuf;

use photos_core::{AppPaths, Db, Library};
use serde::Serialize;
use std::sync::Arc;

use tauri::{Emitter, Manager, RunEvent, State};
use tracing_subscriber::EnvFilter;

/// What the UI needs to reach the embedded server.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerInfo {
    base_url: String,
    token: String,
    data_dir: PathBuf,
    version: &'static str,
}

#[tauri::command]
fn server_info(info: State<'_, ServerInfo>) -> ServerInfo {
    info.inner().clone()
}

/// Show a file or folder in Windows Explorer.
#[tauri::command]
fn show_in_explorer(path: PathBuf) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("{} no longer exists", path.display()));
    }
    use std::os::windows::process::CommandExt;
    // Explorer wants backslashes, and `/select,"<path>"` as one argument with the quotes around
    // the path only. A normally quoted argument (`"/select,<path>"`) makes it open Documents instead.
    let path = path.to_string_lossy().replace('/', "\\");
    let mut cmd = std::process::Command::new("explorer");
    if std::path::Path::new(&path).is_dir() {
        cmd.arg(&path);
    } else {
        cmd.raw_arg(format!("/select,\"{path}\""));
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Open a file with its default Windows app (e.g. a video the viewer can't play).
#[tauri::command]
fn open_with_default_app(path: PathBuf) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("{} no longer exists", path.display()));
    }
    std::process::Command::new("explorer").arg(&path).spawn().map(|_| ()).map_err(|e| e.to_string())
}

/// Opens the Windows window menu at a point in the window, in CSS pixels.
#[tauri::command]
fn show_system_menu(window: tauri::WebviewWindow, x: f64, y: f64) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|e| e.to_string())?;
    let scale = window.scale_factor().unwrap_or(1.0);
    window_chrome::show_system_menu(hwnd.0 as isize, (x * scale).round() as i32, (y * scale).round() as i32);
    Ok(())
}

/// Sets a photo as the desktop background. Windows gets an upright JPEG copy in the data folder,
/// so HEIC and RAW work too, and the original can be moved or deleted later.
#[tauri::command]
async fn set_wallpaper(id: i64, library: State<'_, Library>) -> Result<(), String> {
    let lib = library.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let media = photos_core::media::get(lib.db(), id).map_err(|e| e.to_string())?;
        let picture = photos_core::thumbs::ensure_tv(lib.paths(), &media).map_err(|e| e.to_string())?;
        let wallpaper = lib.paths().data_dir.join("wallpaper.jpg");
        std::fs::copy(&picture, &wallpaper).map_err(|e| e.to_string())?;
        desktop::set_wallpaper(&wallpaper)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Where the page's maximize button is (CSS px), so Windows 11 can show Snap Layouts over it.
#[tauri::command]
fn set_max_button_rect(right: f64, top: f64, width: f64, height: f64) {
    window_chrome::set_max_button_rect(right, top, width, height);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    tauri::Builder::default()
        // Must be first: a second launch just focuses the existing window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        // Size, position and maximized state of the window are kept between launches.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all() - tauri_plugin_window_state::StateFlags::VISIBLE,
                )
                .build(),
        )
        .setup(|app| {
            // The window starts hidden, so it doesn't jump when its saved position is restored.
            if let Some(window) = app.get_webview_window("main") {
                if let Ok(hwnd) = window.hwnd() {
                    window_chrome::install_max_button(app.handle(), hwnd.0 as isize);
                }
                let _ = window.show();
            }
            let paths = loop {
                match AppPaths::from_env()? {
                    Ok(paths) => break paths,
                    // Moved to a drive that isn't plugged in.
                    Err(dir) => {
                        let text = format!(
                            "Memora keeps its library data in {}, which can't be reached right now.\n\n\
                             Connect the drive and choose Retry.",
                            dir.display()
                        );
                        if !desktop::ask_retry("Memora", &text) {
                            std::process::exit(0);
                        }
                    }
                }
            };
            tracing::info!("library data in {}", paths.data_dir.display());
            // After a move: delete the old copy now that the new one is in use.
            photos_core::storage::finish_move(&paths);
            let db = Db::open(&paths.db_path)?;
            let library = Library::start(db, paths.clone())?;
            let handle = app.handle().clone();
            library.subscribe(move |status| {
                let _ = handle.emit("index-progress", status);
            });
            let ai = photos_ml::Ai::start(library.clone(), photos_ml::model_dirs(&paths.home_dir));
            let server = tauri::async_runtime::block_on(photos_server::start_local(library.clone(), ai))?;

            app.manage(ServerInfo {
                base_url: server.base_url(),
                token: server.token.clone(),
                data_dir: paths.data_dir.clone(),
                version: env!("CARGO_PKG_VERSION"),
            });
            app.manage(Arc::new(server));
            app.manage(library);
            app.manage(paths);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            server_info,
            show_in_explorer,
            open_with_default_app,
            show_system_menu,
            set_max_button_rect,
            set_wallpaper,
            data_folder::data_folder,
            data_folder::data_folder_target,
            data_folder::move_data_folder,
            data_folder::restart_app
        ])
        .build(tauri::generate_context!())
        .expect("error while starting Memora")
        .run(|app, event| {
            if let RunEvent::Exit = event
                && let Some(server) = app.try_state::<Arc<photos_server::LocalServer>>()
            {
                // DLNA says goodbye, so TVs drop this PC from their source list right away.
                tauri::async_runtime::block_on(server.shutdown());
            }
        });
}
