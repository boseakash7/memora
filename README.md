<p align="center"><img src="docs/favicon.svg" width="96" alt="Memora logo"></p>

# Memora

**A fast, private, offline photo & video library for Windows, with built-in streaming to your TV.**
Think Google Photos, but everything stays on your PC.

> **Heads-up: this whole app was vibe coded.** I built Memora entirely by prompting an AI coding assistant
> ([Claude Code](https://claude.com/claude-code)) and reviewing the results, without hand-writing the code.
> It works well for me, but it is young software. Expect rough edges, keep backups of your photos
> (Memora never modifies your originals, but still), and please report bugs.

## Features
- Index your existing folders in place. Nothing is moved, copied or uploaded.
- Google Photos-style timeline, viewer, albums, favorites and trash
- Search, map and places from photo GPS, all offline
- Duplicate finder
- HEIC/HEIF, camera RAW (CR2, CR3, NEF, ARW, DNG, RAF, ORF, RW2...) and common video formats; HDR videos are tone mapped for TVs
- People and content search ("dog on a beach"), recognised on your PC
- **TV streaming**: shows up as a media server on smart TVs (DLNA/UPnP) and offers a `/tv` page for the TV browser

## Download
Grab the latest installer from the [Releases page](../../releases/latest) (Windows 10/11, 64-bit),
or visit the [project website](https://boseakash7.github.io/memora/).

The installer includes everything Memora needs (FFmpeg and the recognition models), installs for all users
(Windows asks for permission once) and allows TV sharing through Windows Firewall on private networks.
If an installer is not code-signed, Windows SmartScreen may warn you: choose *More info, then Run anyway*.

## Build from source
Requirements: Rust 1.93+, Node 24+, Tauri CLI 2 (`cargo install tauri-cli --version "^2"`).
For development, FFmpeg must be on `PATH` (`winget install Gyan.FFmpeg`), and the recognition models are fetched
once with `scripts\fetch-models.ps1`.

```powershell
cd ui; npm install; cd ..
cargo tauri dev                                                         # run in development
powershell -ExecutionPolicy Bypass -File scripts\build-installer.ps1    # produce the installer
```

`build-installer.ps1` downloads a pinned FFmpeg build (checked by SHA-256), collects the models and
`DirectML.dll`, and runs `cargo tauri build` with them. It signs the app and installer when a code-signing
certificate is set up; see the top of `scripts\sign.ps1`. Third-party licenses: [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).

## Layout
- `crates/core`: database, indexer, metadata, thumbnails, RAW and HEIF reading, duplicates, places
- `crates/ml`: face and content recognition (ONNX Runtime)
- `crates/server`: local HTTP API, media streaming, DLNA/SSDP, TV page
- `src-tauri`: the desktop shell
- `ui`: Svelte 5 + TypeScript interface and the `/tv` page

## Roadmap
See [ROADMAP.md](ROADMAP.md). Design notes live in [plan.md](plan.md).

## License
MIT, see [LICENSE](LICENSE).
