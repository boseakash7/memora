# Memora roadmap

Memora is an offline, local-first photo and video library for Windows. This roadmap is a living document;
priorities change as people try the app. Have an idea? [Open an issue](../../issues).

## Shipped (v0.1)
- [x] Watch existing folders, index in place (your files are never moved or modified)
- [x] Timeline grid grouped by day/month with a date scrubber, and a full viewer for photos and videos
- [x] HEIC/HEIF, JPEG, PNG, WebP, GIF, TIFF and common video formats (with automatic conversion for unsupported codecs)
- [x] Favorites, albums, Trash (emptying sends files to the Windows Recycle Bin)
- [x] Search by filename, place, album and camera, with date/type filters
- [x] Offline places and map from photo GPS data (no internet, no tile server)
- [x] Duplicate finder (exact and near-duplicate) with one-click cleanup
- [x] TV streaming: DLNA/UPnP media server and a TV-friendly web page on your home network (optional PIN for the web page)
- [x] Light and dark theme

## Next (v0.2): polish and reach
- [ ] Signed installer
- [ ] Bundled FFmpeg so there is nothing else to install
- [ ] Windows Firewall rule created by the installer for TV sharing
- [ ] RAW support (CR2, NEF, ARW, DNG, ...)
- [ ] Verified iPhone HEIC rotation; HDR tone mapping for converted videos
- [ ] Timezone-correct dates for videos
- [ ] Faster first-time indexing for very large libraries

## Soon (v0.3): smart, still offline
- [ ] People: on-device face detection and grouping, rename/merge/hide
- [ ] Natural-language search ("dog on a beach") using an on-device CLIP model
- [ ] Memories: "on this day" and auto-generated highlight collections
- [ ] Bulk edit: fix dates, locations and captions

## Later (v0.4+)
- [ ] Private vault ("Locked folder"): encrypted on disk, password + recovery key, never shown on the LAN or TV
- [ ] Basic editing (crop, rotate, adjust), saved non-destructively
- [ ] Windows Hello unlock for the vault
- [ ] Back up / sync to an external drive or NAS
- [ ] Localization

## Ideas, not committed
- macOS and Linux builds (Tauri supports them; DLNA and Recycle Bin code need per-platform work)
- Shared albums over the LAN

## Principles
1. **Offline first.** No accounts, no cloud, no telemetry. Nothing leaves your PC unless you turn on TV sharing for your own network.
2. **Your files stay yours.** Memora indexes in place and never moves or edits originals (the future vault is the one explicit, opt-in exception).
3. **Fast on huge libraries.** Thumbnails are cached; indexing resumes after a restart.
