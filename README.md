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
- HEIC/HEIF and common video formats
- **TV streaming**: shows up as a media server on smart TVs (DLNA/UPnP) and offers a `/tv` page for the TV browser

## Download
Grab the latest installer from the [Releases page](../../releases/latest) (Windows 10/11, 64-bit),
or visit the [project website](https://OWNER.github.io/Memora/).

Memora uses **FFmpeg** for videos and HEIC. Install it and make sure `ffmpeg` and `ffprobe` are on your `PATH`
(`winget install Gyan.FFmpeg`).
The installer is not code-signed yet, so Windows SmartScreen may warn you: choose *More info, then Run anyway*.

## Build from source
Requirements: Rust 1.93+, Node 24+, Tauri CLI 2 (`cargo install tauri-cli --version "^2"`), FFmpeg on PATH.

```powershell
cd ui; npm install; cd ..
cargo tauri dev      # run in development
cargo tauri build    # produce the installer
```

## Layout
- `crates/core`: database, indexer, metadata, thumbnails, duplicates, places
- `crates/server`: local HTTP API, media streaming, DLNA/SSDP, TV page
- `src-tauri`: the desktop shell
- `ui`: Svelte 5 + TypeScript interface and the `/tv` page

## Roadmap
See [ROADMAP.md](ROADMAP.md). Design notes live in [plan.md](plan.md).

## License
MIT, see [LICENSE](LICENSE).
