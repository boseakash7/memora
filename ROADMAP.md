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

## Shipped (v0.2): polish, reach and smart search
- [x] Bundled FFmpeg and recognition models, so there is nothing else to install
- [x] Windows Firewall rule created by the installer for TV sharing (private networks only)
- [x] RAW support (CR2, CR3, NEF, ARW, DNG, RAF, ORF, RW2, PEF, ...)
- [x] Verified iPhone HEIC rotation; HDR tone mapping for converted videos and their thumbnails
- [x] Timezone-correct dates for videos
- [x] Faster first-time indexing for very large libraries
- [x] People: on-device face detection and grouping, rename/merge/hide
- [x] Natural-language search ("dog on a beach") using an on-device CLIP model

## Soon (v0.3): organize and carry your library
- [x] Custom title bar: Memora's own top bar with window buttons instead of the Windows title bar (snap, resize and the Windows window menu keep working)
- [x] Right-click menus for photos and videos (favorite, album, archive, copy image or path, show in Explorer, open with, set as desktop background, trash), also for a selection, in the viewer and in Trash
- [x] Archive: hide photos from everywhere except the Archive page; an Archive row on the TV page and an Archive folder for DLNA (can be switched off)
- [x] Your library goes with your photos: a hidden `.memora` folder in each watched folder keeps favorites, albums, archive, trash and people names, so another PC that adds the same folder (external drive, NAS or a synced folder) shows the same library and stays in sync
- [x] The `.memora` folder also stores the finished index, so the second PC skips the slow indexing and recognition
- [x] Move Memora's data and cache to another drive; "Locate folder" when a drive letter changes
- [x] Back up and restore library info as one file

## Later (v0.4+)
- [ ] Signed installer, so Windows SmartScreen doesn't warn (moved from v0.2: the build already signs the app and installer once a code-signing certificate is set up; the certificate itself is still to get)
- [ ] Private vault ("Locked folder"): encrypted on disk, password + recovery key, never shown on the LAN or TV
- [ ] Memories: "on this day" and auto-generated highlight collections
- [ ] Bulk edit: fix dates, locations and captions
- [ ] Basic editing (crop, rotate, adjust), saved non-destructively
- [ ] Windows Hello unlock for the vault
- [ ] Back up the photos themselves to an external drive or NAS
- [ ] Localization

## Ideas, not committed
- macOS and Linux builds (Tauri supports them; DLNA and Recycle Bin code need per-platform work)
- Shared albums over the LAN

## Principles
1. **Offline first.** No accounts, no cloud, no telemetry. Nothing leaves your PC unless you turn on TV sharing for your own network.
2. **Your files stay yours.** Memora indexes in place and never moves or edits originals (the future vault is the one explicit, opt-in exception). The only thing it writes next to your photos is the hidden `.memora` folder with your library info, and it asks first.
3. **Fast on huge libraries.** Thumbnails are cached; indexing resumes after a restart.
