//! Downloading model files: over HTTPS from a pinned URL into `<file>.part`, continuing an
//! interrupted download where it stopped, then checked against its SHA-256 and renamed into
//! place, so a half-written or damaged file is never loaded. Only the model files are
//! fetched; nothing about the user or their photos is sent.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};
use ureq::Agent;
use ureq::http::StatusCode;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use crate::models::ModelFile;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("stopped")]
    Stopped,
    #[error("couldn't reach the download server ({0}). Check the internet connection and try again.")]
    Network(String),
    #[error("the download server answered {0}; try again later")]
    Status(u16),
    #[error("the downloaded file was damaged; try again")]
    Damaged,
    #[error("couldn't save the model file: {0}")]
    Io(#[from] std::io::Error),
}

pub fn agent() -> Agent {
    Agent::new_with_config(
        Agent::config_builder()
            .proxy(ureq::Proxy::try_from_env())
            .https_only(true)
            .tls_config(TlsConfig::builder().provider(TlsProvider::NativeTls).root_certs(RootCerts::PlatformVerifier).build())
            .user_agent(concat!("Memora/", env!("CARGO_PKG_VERSION")))
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build(),
    )
}

fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// How much of `file` is already downloaded into `dir` (finished or in its `.part`).
pub fn have(dir: &Path, file: &ModelFile) -> u64 {
    let dest = dir.join(file.path);
    if dest.is_file() {
        return file.size;
    }
    std::fs::metadata(part_path(&dest)).map(|m| m.len().min(file.size)).unwrap_or(0)
}

/// Remove an unfinished download.
pub fn discard(dir: &Path, file: &ModelFile) {
    let _ = std::fs::remove_file(part_path(&dir.join(file.path)));
}

/// Download `file` into `dir`, reporting the bytes written so far. Returns
/// [`DownloadError::Stopped`] (keeping what it has) once `stop` is set.
pub fn fetch(agent: &Agent, dir: &Path, file: &ModelFile, stop: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<(), DownloadError> {
    let dest = dir.join(file.path);
    if dest.is_file() {
        progress(file.size);
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let part = part_path(&dest);
    let mut out = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&part)?;
    let mut got = out.metadata()?.len();
    if got > file.size {
        out.set_len(0)?;
        got = 0;
    }
    if got < file.size {
        let mut req = agent.get(file.url);
        if got > 0 {
            req = req.header("Range", format!("bytes={got}-"));
        }
        let resp = req.call().map_err(|e| DownloadError::Network(e.to_string()))?;
        match resp.status() {
            StatusCode::PARTIAL_CONTENT => {}
            StatusCode::OK => {
                // The server sent the whole file: start over.
                out.set_len(0)?;
                got = 0;
            }
            StatusCode::RANGE_NOT_SATISFIABLE => {
                out.set_len(0)?;
                return Err(DownloadError::Damaged);
            }
            s => return Err(DownloadError::Status(s.as_u16())),
        }
        out.seek(SeekFrom::Start(got))?;
        progress(got);
        let mut body = resp.into_body().into_with_config().limit(file.size - got).reader();
        let mut buf = vec![0u8; 256 * 1024];
        while got < file.size {
            if stop.load(Ordering::Relaxed) {
                out.flush()?;
                return Err(DownloadError::Stopped);
            }
            let n = body.read(&mut buf).map_err(|e| DownloadError::Network(e.to_string()))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            got += n as u64;
            progress(got);
        }
        out.flush()?;
        if got < file.size {
            return Err(DownloadError::Network("the connection closed early".into()));
        }
    }
    out.seek(SeekFrom::Start(0))?;
    let ok = sha256(&mut out)? == file.sha256;
    drop(out);
    if !ok {
        let _ = std::fs::remove_file(&part);
        return Err(DownloadError::Damaged);
    }
    std::fs::rename(&part, &dest)?;
    Ok(())
}

fn sha256(f: &mut File) -> std::io::Result<String> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;

    /// A file served over plain HTTP by a tiny local server that honours `Range`, so no
    /// internet is needed. Returns the URL; the server answers `requests` requests.
    fn serve(body: &'static [u8], requests: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(requests) {
                let mut stream = stream.unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut from = 0usize;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some(r) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        from = r.trim().trim_end_matches('-').parse().unwrap();
                    }
                }
                let part = &body[from..];
                let status = if from > 0 { "206 Partial Content" } else { "200 OK" };
                write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", part.len()).unwrap();
                stream.write_all(part).unwrap();
            }
        });
        format!("http://{addr}/model.onnx")
    }

    fn plain_agent() -> Agent {
        Agent::new_with_config(Agent::config_builder().http_status_as_error(false).build())
    }

    fn file(url: &str, body: &[u8], sha: &'static str) -> ModelFile {
        ModelFile { path: "m/model.onnx", url: Box::leak(url.to_string().into_boxed_str()), size: body.len() as u64, sha256: sha }
    }

    const BODY: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

    fn sha_of(b: &[u8]) -> &'static str {
        let s: String = Sha256::digest(b).iter().map(|b| format!("{b:02x}")).collect();
        Box::leak(s.into_boxed_str())
    }

    #[test]
    fn continues_an_interrupted_download_and_checks_it() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&serve(BODY, 1), BODY, sha_of(BODY));
        // Half of it came down before the app closed.
        std::fs::create_dir_all(dir.path().join("m")).unwrap();
        std::fs::write(dir.path().join("m/model.onnx.part"), &BODY[..10]).unwrap();
        assert_eq!(have(dir.path(), &f), 10);
        let mut last = 0;
        fetch(&plain_agent(), dir.path(), &f, &AtomicBool::new(false), &mut |n| last = n).unwrap();
        assert_eq!(std::fs::read(dir.path().join("m/model.onnx")).unwrap(), BODY);
        assert_eq!(last, BODY.len() as u64);
        assert!(!dir.path().join("m/model.onnx.part").exists());
    }

    #[test]
    fn a_damaged_download_is_thrown_away() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&serve(BODY, 1), BODY, sha_of(b"something else entirely, same len!!!"));
        let err = fetch(&plain_agent(), dir.path(), &f, &AtomicBool::new(false), &mut |_| {}).unwrap_err();
        assert!(matches!(err, DownloadError::Damaged), "{err}");
        assert!(!dir.path().join("m/model.onnx").exists() && !dir.path().join("m/model.onnx.part").exists());
    }

    /// The real thing, over HTTPS: the smallest model (228 KB). Needs the internet:
    /// `cargo test -p photos-ml -- --ignored downloads_from_hugging_face`
    #[test]
    #[ignore]
    fn downloads_from_hugging_face() {
        let dir = tempfile::tempdir().unwrap();
        let f = &crate::models::FACES[0];
        fetch(&agent(), dir.path(), f, &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(std::fs::metadata(dir.path().join(f.path)).unwrap().len(), f.size);
    }

    #[test]
    fn stopping_keeps_what_came_down() {
        let dir = tempfile::tempdir().unwrap();
        let f = file(&serve(BODY, 1), BODY, sha_of(BODY));
        let err = fetch(&plain_agent(), dir.path(), &f, &AtomicBool::new(true), &mut |_| {}).unwrap_err();
        assert!(matches!(err, DownloadError::Stopped));
        assert!(dir.path().join("m/model.onnx.part").exists() && !dir.path().join("m/model.onnx").exists());
    }
}
