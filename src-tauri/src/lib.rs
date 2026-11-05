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
        .setup(|app| {
            let paths = AppPaths::from_env()?;
            tracing::info!("library data in {}", paths.data_dir.display());
            let db = Db::open(&paths.db_path)?;
            let library = Library::start(db, paths.clone())?;
            let handle = app.handle().clone();
            library.subscribe(move |status| {
                let _ = handle.emit("index-progress", status);
            });
            let server = tauri::async_runtime::block_on(photos_server::start_local(library.clone()))?;

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
        .invoke_handler(tauri::generate_handler![server_info, show_in_explorer, open_with_default_app])
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
