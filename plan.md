# Plan: Offline "Google Photos"-style app for Windows (Rust + Tauri) with TV streaming

## Context
The project at `I:\My Learnings\Rust\photos` is a fresh `cargo new` (empty `main.rs`, edition 2024, no deps).
Goal: a local, offline-only Windows desktop photo/video library that looks and feels like Google Photos web,
and can stream photos/videos to a TV over the home network.

Decisions confirmed with the user:
- **UI**: Tauri 2 desktop app (Rust backend + web UI).
- **Library**: watch existing folders, index in place, never move or edit files. The one thing written into a watched folder is a hidden `.memora` folder with the library info (asked when the folder is added; added 2026-11-11).
- **TV**: DLNA/UPnP media server (TV browses the PC with its remote) **and** a TV-friendly web page on the LAN.
- **Extras**: map/places (GPS), offline face grouping, offline object/scene search (CLIP), duplicate detection.
- **v0.3 (decided 2026-11-11)**: self-designed title bar instead of the Windows one; custom right-click menus; Archive that shows items only in the Archive section (plus a TV/DLNA Archive); library info that travels with the photos between PCs (`.memora`, trash synced too, index shared by default). Details in "Planned: Custom title bar, …" after the Progress section.
- Toolchain present: Rust 1.93, Node 24 / npm 11, tauri-cli 2.9.6, FFmpeg/ffprobe 7.1 on PATH.

Offline rule: the app makes **no internet requests**, with one exception the user asks for: downloading
the AI features they pick (first-start welcome or Settings › AI features), from pinned Hugging Face revisions
checked by SHA-256 (decided 2026-11-19). Nothing about the user or their photos is sent. The full offline
installer still bundles every model, and map data is always bundled.

## Architecture
One Rust process (the Tauri app) hosts:
1. **Tauri window** running the web UI (Svelte 5 + TypeScript + Vite). From v0.3 the window has no Windows title bar; the app's top bar is the title bar (own window buttons, drag areas).
2. **Embedded axum HTTP server**, two listeners:
   - `127.0.0.1:<port>`: full JSON API + media for the desktop UI (the UI calls it with `fetch`; video `<video src>` uses HTTP range requests). Protected by a random per-launch token passed to the UI.
   - `0.0.0.0:<lan-port>` (off by default, toggle in Settings): **read-only** media, `/tv` web UI, and DLNA endpoints. Optional PIN for `/tv`. Trashed items are never exposed.
3. **SSDP responder** (UDP multicast 239.255.255.250:1900) advertising the DLNA MediaServer while LAN sharing is on.
4. **Background job workers** (tokio + rayon) for scanning, thumbnails, hashing, faces, CLIP.
5. **Library info sync** (v0.3): per watched folder, a `.memora` folder with one append-only change log and one index snapshot per PC. The local `library.db` stays the working copy; changes are written to an outbox and flushed to `.memora`, and other PCs' logs are merged in when they change (file watcher).

Tauri commands are used only for native things (folder picker, open in Explorer, server port/token, LAN toggle); everything else goes through the HTTP API so the desktop UI and the TV page share one code path.

## Workspace layout
```
Cargo.toml                 # [workspace] members
crates/
  core/      # DB, models, indexer, watcher, metadata, thumbnails, dedupe, geo, jobs
  ml/        # ort (ONNX Runtime) face detection/embedding, CLIP, clustering
  server/    # axum API, media streaming, transcode, TV page, DLNA + SSDP
src-tauri/   # Tauri 2 app: wires core+ml+server, commands, tray, settings
ui/          # Svelte 5 + Vite + TS desktop UI (and /tv page as a second entry)
assets/      # bundled: world GeoJSON, model files (git-ignored / LFS), icons
```
The existing root `src/main.rs` and package section are replaced by the workspace.

## Key crates
- Tauri: `tauri` 2, `tauri-plugin-dialog`, `tauri-plugin-single-instance`; v0.3 adds `tauri-plugin-window-state`, `tauri-plugin-clipboard-manager` and the `windows` crate (window subclass for Snap Layouts, system menu, desktop wallpaper)
- Server: `axum`, `tokio`, `tower-http` (ServeFile with Range, CORS for the localhost UI), `quick-xml` (DLNA SOAP/DIDL), `socket2` (multicast), `local-ip-address`, `uuid`
- DB: `rusqlite` (feature `bundled`, with FTS5) + `r2d2_sqlite` pool, `refinery` or hand-written migrations
- Indexing: `walkdir`, `notify` + `notify-debouncer-full`, `blake3`, `kamadak-exif`, `chrono`
- Images: `image`, `fast_image_resize`, `image_hasher` (perceptual hash); HEIC/HEIF and RAW previews via FFmpeg fallback
- Video: ffprobe/ffmpeg invoked as subprocesses (bundled as a Tauri sidecar for the installer; PATH used in dev)
- Geo: `reverse_geocoder` (embedded GeoNames cities, fully offline)
- ML: `ort` (ONNX Runtime, DirectML execution provider for GPU, CPU fallback), `ndarray`, `tokenizers` (CLIP BPE)
- UI: Svelte 5, Vite, TypeScript, MapLibre GL JS (offline, no tile server), a virtualized justified-layout grid

## Data model (SQLite at `%LOCALAPPDATA%\Memora\library.db`; from v0.3 the data folder can be moved, its location kept in `%LOCALAPPDATA%\Memora\location.json`)
Original design below; what was actually built is under "Changes from the original plan". v0.3 adds `media.archived_at`, `albums.uid`, `persons.uid`, an `outbox` of changes not yet written to `.memora`, and per-folder sync cursors (migrations 9 and 10).
- `folders(id, path, enabled)`: watched roots
- `media(id, folder_id, path UNIQUE, size, mtime, blake3, kind[photo|video], mime, width, height, duration_ms, orientation, taken_at, tz_offset, camera_make, camera_model, lat, lon, place_id, phash, favorite, trashed_at, missing)`
- `media_fts`: FTS5 over filename, folder, camera, place names, album names
- `albums(id, name, cover_media_id, created_at)`, `album_items(album_id, media_id, position)`
- `places(id, country, admin1, city)`
- `faces(id, media_id, x, y, w, h, score, embedding BLOB, person_id)`, `persons(id, name, cover_face_id, hidden)`
- `clip_embeddings(media_id, vec BLOB)`: 512-d f32 vectors, L2-normalized
- `jobs(id, media_id, kind, state, attempts, error)`: persistent queue so indexing resumes after restart
- `settings(key, value)`

Cache: thumbnails at `%LOCALAPPDATA%\Memora\cache\{256,1080}\ab\<blake3>.webp`; video poster frames the same way.

## Feature details

### Indexing and watching (crates/core)
- Add folder → full `walkdir` scan → upsert `media` rows (skip unchanged by size+mtime) → enqueue jobs.
- `notify` debounced watcher per root: create/modify → re-index, delete → mark `missing`, rename → update path (match by blake3).
- Job pipeline per item, in order: `metadata` (EXIF via kamadak-exif, ffprobe JSON for video; taken_at fallback to file mtime) → `thumbnail` → `phash` → `geocode` → `faces` → `clip`. Cheap stages run first, so the grid fills quickly while ML runs later at low priority.
- Progress is pushed to the UI as Tauri events (`index-progress`).

### Desktop UI (ui/)
- **Photos**: virtualized justified grid grouped by day/month with sticky headers, plus a date scrubber on the right edge (like Google Photos). Multi-select with shift-click.
- **Viewer/lightbox**: keyboard nav, zoom/pan, info panel (EXIF, map pin, people, albums), video playback via `<video>` with range streaming, favorite/trash/add-to-album, "Open in Explorer".
- **Explore**: People (face clusters, rename, merge, hide), Places, Map, Videos, Favorites.
- **Search bar**: free text → combines FTS (filename/place/album/person names) + CLIP semantic results; filters for date range, type, camera.
- **Albums**: create, add/remove, reorder, cover.
- **Archive** (v0.3): archived items are shown only on the Archive page; hidden from the timeline, favorites, albums, search, people, places, map and duplicates. Shift+A.
- **Utilities**: Duplicates review (exact + near), Trash (30-day soft delete; "empty trash" sends files to the Windows Recycle Bin via the `trash` crate, never a permanent delete).
- **Settings**: watched folders (with "Locate folder…" and "Keep library info in this folder"), LAN sharing toggle + PIN + device name + Archive on TV, transcoding on/off, ML on/off and GPU/CPU choice, data folder location (v0.3: moves the database and cache together), backup/restore of library info.
- **Title bar** (v0.3): the top bar (logo, search, indexing chip, settings) doubles as the window title bar with minimize/maximize/close; also in the viewer.
- **Right-click menus** (v0.3): per photo, video, selection, viewer and Trash; all actions shared with the selection bar and viewer toolbar (`lib/actions.ts`).
- Light/dark theme following Windows.

### Media serving and transcoding (crates/server)
- `GET /media/:id/original`: file with Range support (tower-http `ServeFile`).
- `GET /media/:id/thumb/:size`: cached WebP, generated on demand if missing.
- `GET /media/:id/display`: HEIC/RAW/huge images converted to a JPEG sized for the screen (cached).
- `GET /media/:id/stream.ts`: on-the-fly FFmpeg transcode to H.264/AAC MPEG-TS for videos the TV can't play (HEVC, MKV, high bit-depth). Used by the TV paths; the desktop plays originals when WebView2 supports the codec.

### DLNA/UPnP MediaServer (crates/server/dlna)
Hand-written (no mature Rust crate exists):
- **SSDP**: answer `M-SEARCH` for `ssdp:all`, `upnp:rootdevice`, `MediaServer:1`, `ContentDirectory:1`; send `NOTIFY ssdp:alive` periodically and `ssdp:byebye` on shutdown or when sharing is turned off.
- `GET /dlna/description.xml`: device description (friendlyName from Settings, stable UUID stored in settings).
- **ContentDirectory:1** SOAP: `Browse` (BrowseMetadata / BrowseDirectChildren with paging), `GetSystemUpdateID`, `GetSortCapabilities`, `GetSearchCapabilities`; Samsung `X_GetFeatureList` stub.
- **ConnectionManager:1**: `GetProtocolInfo` listing JPEG/PNG/MP4/MKV/TS sources.
- Virtual tree: `Timeline/<Year>/<Month>`, `Albums/<name>`, `People/<name>`, `Places/<country>/<city>`, `Favorites`, `Videos`, `Folders/...`, and from v0.3 `Archive` (the only folder that lists archived items; can be switched off).
- DIDL-Lite items carry `res` entries with `protocolInfo` + DLNA.ORG_PN/OP/FLAGS, a thumbnail `res` (JPEG_TN) and `upnp:albumArtURI`; videos get both the original and a transcoded `res`.
- Responses send `transferMode.dlna.org` and `contentFeatures.dlna.org` headers; honour `Range` and `TimeSeekRange.dlna.org` (seek for transcoded streams via `ffmpeg -ss`).

### TV web page (`/tv` on the LAN listener)
- Separate Vite entry, large tiles, D-pad/arrow-key focus navigation, Enter to open, Back to return.
- Sections: Recent, Albums, People, Places, Favorites, Videos, and from v0.3 Archive (the only place archived items appear; behind the PIN if one is set); fullscreen viewer and slideshow with an interval setting.
- Videos use the transcoded stream when the TV browser can't play the original.
- Desktop shows the LAN URL and a QR code to open it.

### Map and places (offline)
- Reverse geocoding: `reverse_geocoder` maps lat/lon → city/admin/country, stored in `places`.
- Map view: MapLibre GL with a bundled Natural Earth world-boundaries GeoJSON as a basemap (a few MB, no tiles), plus clustered photo markers from `/api/map/points`. Optional: if the user drops a `.pmtiles` file into the maps folder, use it as a detailed basemap.

### Face grouping (crates/ml)
- Detection: **YuNet** ONNX (OpenCV Zoo, Apache-2.0). Embedding: **SFace** ONNX (Apache-2.0), 128-d, after 5-point alignment.
- Clustering: incremental. New face joins the nearest person if cosine similarity > threshold, else goes to "unassigned"; a periodic DBSCAN pass over unassigned faces creates new persons. Manual merge/split/rename always overrides auto clustering.
- Runs through `ort` with DirectML (GPU) when available, else CPU, with a capped thread count so the PC stays responsive.

### Object/scene search (crates/ml)
- **CLIP ViT-B/32** image + text encoders as ONNX (quantized ~150 MB), tokenizer via `tokenizers`.
- Image embeddings computed in the job pipeline; text query embedded at search time.
- Search: brute-force SIMD cosine over an in-memory `Vec<f32>` matrix loaded at startup (fine for ~200k items). Swap in an HNSW index later if needed.

### Duplicate detection (crates/core)
- Exact: same `blake3`.
- Near: 64-bit perceptual hash (`image_hasher`, gradient/DCT), BK-tree with Hamming distance ≤ ~6; videos compared by poster-frame hash + duration.
- UI groups duplicates, suggests the keeper (highest resolution, then has EXIF, then oldest), one click moves the rest to Trash.

## Build phases (each one ends with a runnable app)
1. **Skeleton**: workspace, Tauri 2 + Svelte app, SQLite + migrations, embedded axum on localhost, settings page with folder picker.
2. **Library core**: scan + watcher, EXIF/ffprobe metadata, thumbnails, timeline grid with date groups and scrubber, viewer with video playback.
3. **Organize**: favorites, albums, trash/Recycle Bin, FTS search with filters, info panel.
4. **Places & duplicates**: reverse geocoding, map view, phash + duplicate review UI.
5. **TV**: LAN listener, `/tv` page, transcode endpoint, DLNA MediaServer + SSDP, LAN URL/QR in settings.
6. **AI**: ort setup with DirectML/CPU, faces + People UI, CLIP + semantic search.
7. **Packaging**: bundle ffmpeg sidecar, models, GeoJSON; NSIS/MSI installer via `cargo tauri build`; Windows Firewall rule prompt for the LAN port.
8. **v0.3, organize and carry your library** (added 2026-11-11): custom title bar, right-click menus, Archive (incl. TV/DLNA Archive), movable data folder + "Locate folder…", `.memora` library info sync with shared index, backup file.
9. **Private vault** (v0.4, started 2026-11-18).
10. **Memories** (v0.4, started 2026-11-18): on this day, trips, people, best of a month/year; Photos carousel, Memories page, TV row.

## Verification
- `cargo test --workspace`: unit tests for EXIF/date parsing, DIDL-Lite/SOAP generation (against sample TV requests), SSDP response format, phash/BK-tree, clustering, Range header handling.
- `cargo tauri dev`: add a test folder with JPEG/HEIC/PNG/MP4/MOV/MKV samples (some with GPS, faces, duplicates); confirm grid, viewer, video seek, albums, trash, and that new files dropped in the folder appear without restarting.
- LAN/TV: enable sharing, then
  - open `http://<pc-ip>:<port>/tv` on a phone and on the TV browser, navigate with arrow keys;
  - confirm the PC shows up as a media server in the TV's source list (and in VLC → Local Network → UPnP as a quick check on the PC); browse Timeline/Albums, play a photo, an MP4, and an HEVC/MKV through the transcoded stream, and seek.
- Offline check: run with the network adapter's internet disabled (LAN only) and confirm every feature works; check the app makes no outbound connections (Resource Monitor).
- AI: search "beach", "dog", "car" on a sample set; confirm the same person's faces cluster together and merge/rename persists.
- v0.3:
  - Title bar: move, snap, resize from every edge, double-click maximize, Alt+Space menu, maximized on both monitors, light/dark/inactive colours, viewer and dialogs open.
  - Right-click menus on photos, videos, a selection, in the viewer and in Trash; keyboard (Shift+F10, arrows, Esc).
  - Archive: an archived photo is gone from every page except Archive (timeline, favorites, album, search, people, places, map, duplicates), shows in the TV page Archive row and the DLNA Archive folder only, and comes back in place after Unarchive.
  - Two PCs (or two data folders on one PC with `PHOTOS_DATA_DIR`) sharing one photo folder under different drive letters: favorite, archive, album, rename a person and trash on one; all appear on the other; the second one skips probing and recognition thanks to the index snapshot; same check through a OneDrive folder and with the drive unplugged mid-change.

## Effort estimate (rough)
Assumes Claude writes the code and the user tests each phase (real photo library, real TV) before the next one starts.
Expected size: ~10–12k lines of Rust plus ~6–8k lines of Svelte/TypeScript.

| Phase | Calendar days | Input tokens (mostly cached context) | Output tokens |
|---|---|---|---|
| 1. Skeleton | 1 | 3–6M | 40–80k |
| 2. Library core | 2–3 | 10–20M | 100–200k |
| 3. Organize | 2 | 8–15M | 80–150k |
| 4. Places & duplicates | 1–2 | 5–10M | 50–100k |
| 5. TV (DLNA + /tv) | 3–4 | 10–25M | 100–200k |
| 6. AI (faces + CLIP) | 3–5 | 10–25M | 100–200k |
| 7. Packaging | 1–2 | 3–8M | 30–60k |
| **Total** | **~13–19 days** | **~50–110M** | **~0.5–1M** |

Biggest unknowns: TV-specific DLNA quirks (each brand behaves differently) and getting the ONNX models running well on DirectML.

## Progress (updated 2026-11-11)

**Status: Phases 1–7 built, plus the v0.2 roadmap items (RAW, video time zones, HDR, faster indexing). Phase 5 (TV) still needs a test on the real Samsung TV; the installer still needs a code-signing certificate and a test install. v0.3 is built: title bar, right-click menus, Archive, data folder move, `.memora` library info and index snapshot, and the library info backup file (see "Planned: Custom title bar, …" after the Progress section). Next: the private vault.**

| Phase | Status |
|---|---|
| 1. Skeleton | ✅ Done |
| 2. Library core | ✅ Done |
| 3. Organize | ✅ Done |
| 4. Places & duplicates | ✅ Done |
| 5. TV (DLNA + /tv) | ✅ Built, awaiting a real-TV test |
| 6. AI (faces + CLIP) | ✅ Done |
| 7. Packaging | ✅ Built (v0.2), unsigned until a certificate is set up |
| 8. v0.3: title bar, right-click menus, Archive, portable library | ✅ Built (2026-11-18): custom title bar, right-click menus, Archive, data folder move and Locate folder, `.memora` library info sync and index snapshot, backup file |
| 9. v0.4: Private vault | ✅ Built 2026-11-18 (per-row encryption instead of SQLCipher); locking on Windows lock and on minimize not tried yet |
| 10. v0.4: Memories | ✅ Built 2026-11-18 (on this day, trips, people, best of; Photos carousel, Memories page, story player, TV row) |
| 11. v0.4: Photo editing | ✅ Built 2026-11-18 (crop/rotate/straighten, adjustments, 26 filters, Enhance, background blur; non-destructive, synced) |

### Completed

**Phase 1: Skeleton**
- Cargo workspace (`crates/core`, `crates/server`, `src-tauri`) and Svelte 5 + Vite + TS UI in `ui/`.
- SQLite with WAL + versioned migrations (`PRAGMA user_version`), r2d2 connection pool.
- Embedded axum server on `127.0.0.1:<random port>`, per-launch token (header or `?token=`), CORS for the UI origins.
- Settings stored per key with validation (theme, LAN port, PIN, device name, ML/transcode flags).
- Watched folders: add (folder picker), remove, overlap check, "not available" badge for unplugged drives.
- App shell: top bar, sidebar, Settings page, light/dark/system theme, single-instance, "Show in Explorer".

**Phase 2: Library core**
- `media` table (migration 2) with size/mtime, kind, mime, dimensions, duration, orientation, taken-at + timezone, camera, GPS, plus columns for later phases (favorite, trashed_at, blake3, phash).
- Scanner: walks folders (skips hidden/system folders), diffs against the DB by size+mtime, marks deleted files as missing (not when a folder couldn't be read or the drive is unplugged), restores them when they come back.
- File watcher (notify, 2 s debounce): new, changed, deleted and renamed files and folders. Renames keep the same row (so favorites/albums will survive); buffer overflow triggers a full rescan. A full rescan also runs at startup.
- Background pipeline on a worker pool (half the CPU cores, max 6): metadata for everything first (grid fills fast), then thumbnails. Progress resumes after restart via `media.stage`.
- Metadata: EXIF (date + offset, orientation, make/model, GPS) via kamadak-exif; videos via ffprobe (size, rotation, duration, Apple/Android date, make/model and location tags).
- Thumbnails: fast scaled JPEG decode, EXIF rotation applied, made on demand if the pipeline hasn't reached an item yet.
- HEIC/HEIF/AVIF and video frames decoded with FFmpeg (hidden console window, 60 s timeout).
- API: `/api/timeline`, `/api/media/{id}`, `.../thumb`, `.../display` (JPEG copy for TIFF/HEIC), `.../original` (HTTP range for video seeking), `/api/library/status`, `/api/library/rescan`.
- Live progress pushed to the UI (`index-progress` event): top-bar chip, library card in Settings with counts and Rescan.
- Photos page: virtualized justified grid grouped by day; small days share a line; video duration badges; broken-file tiles; keeps scroll position while new photos arrive; date scrubber with years, month dots and a month bubble.
- Viewer: thumbnail-then-full-image loading, wheel/double-click zoom, drag to pan, ←/→/Esc/I keys, video playback, "can't play this format" fallback with "Open in default player", info panel (date, size, camera, GPS, path).
- Fixes after testing with a real phone library (`I:\My Photos`: 2,267 HEIC, 428 HEVC MP4, 359 JPG):
  - Portrait/rotated videos overflowed the viewer; videos are now sized to fit like photos.
  - Tiled HEIC photos (48 × 512px tiles) showed only their first tile. They are now stitched with an FFmpeg `xstack` filter built from `ffprobe -show_stream_groups`; size comes from the probe; rotation from the display matrix, else EXIF. Migration 3 re-processes existing HEIC/HEIF/AVIF, and the UI's `THUMB_REVISION` busts cached thumbnails.
  - HEIC decoding runs FFmpeg single-threaded (≈3× faster when several files decode in parallel); `photos-core` is optimized in dev builds.
  - Dot-files are skipped (Android's `.trashed-…` deleted items, macOS `._…`).
  - Verified: all 3,051 items indexed with no errors; 4K60 HEVC plays with 0 dropped frames (needs the Microsoft "HEVC Video Extensions", which this PC has).
- Tests: 27 Rust tests (scan, watcher, renames, EXIF, ffprobe parsing, thumbnails, end-to-end indexing, HTTP endpoints incl. range requests). Verified in the running app with ~100 sample photos/videos.

**Phase 3: Organize**
- Migration 4: `albums`, `album_items`, indexes for favorites/trash, and the `media_fts` FTS5 table (path, camera, album names), kept current by triggers.
- Core: `albums.rs` (create, rename, delete, add/remove items, cover), `organize.rs` (favorites, trash/restore, empty trash, 30-day auto-purge), and `media::query` (one query for the Photos, Favorites, Trash, album and search grids; text, date range, photo/video and camera filters).
- Trash is a soft delete (`trashed_at`). Emptying it sends the files to the Windows Recycle Bin (`trash` crate; checked on C: and I:) and forgets the items. Only items already in the trash can be deleted; a file that is gone is only dropped if its folder is reachable. Expired items (30 days) are purged at startup and every 6 hours.
- API: `/api/timeline` takes `scope`, `album`, `q`, `from`, `to`, `kind`, `camera`; plus `/cameras`, `/media/favorite|trash|restore`, `/media/{id}/albums`, `/trash/empty`, `/albums` (+ `/{id}`, `/{id}/items`). Changes bump the library revision so open views reload.
- UI: multi-select (checkmark, Shift-click range, select a day, Ctrl+A, Esc) with a selection bar; favorites star in grid/viewer; Favorites, Albums, album, Trash and Search pages; album picker; confirm/prompt dialogs; toasts with Undo; viewer actions (favorite, add to album, trash/restore/delete, use as cover) and an Albums row in the info panel; search box with live search and filters (type, camera, date range).
- Verified: 36 Rust tests (search incl. accents/operators, album and trash rules, API flow); migration 4 run on a copy of the real library (3,051 items, 37 ms); `svelte-check` clean. Not yet clicked through in the running app.

**Phase 4: Places & duplicates**
- Pipeline stage 3 `HASH` after thumbnails: `blake3` of the file, a 64-bit perceptual hash (DCT of the cached thumbnail, so videos use their poster frame; flat pictures get none) and reverse geocoding. Migration 5 leaves existing items at stage 2, so they are hashed once in the background after the upgrade (3,051 items on the real library).
- Places: `reverse_geocoder` (embedded GeoNames, offline); nearest city within 50 km, else no place. `places` table + `media.place_id`; countries are stored as ISO codes and named in the UI with `Intl.DisplayNames`. Place names are in the search index (migration 5 rebuilds `media_fts`).
- Duplicates: exact groups by `blake3`; near groups by BK-tree on the perceptual hash (Hamming <= 6) with a similar aspect ratio, videos also a similar duration. Suggested keeper: highest resolution, then has EXIF/GPS, then oldest. Computed on request, nothing stored.
- API: `/api/places`, `/api/places/{id}`, `/api/map/points` (compact `[id, lat, lon]`), `/api/duplicates`; `/api/timeline` also takes `place` and `bbox` (`west,south,east,north`, west > east crosses the antimeridian).
- UI: Explore hub (Map, Places, Videos), Places page (by country) and place view, Map page (MapLibre, clustered markers, and a grid of the photos in the visible area), Utilities hub, Duplicates page (per-group "keep this one instead", trash the others with Undo, trash all). The map code loads only when the map is opened (separate chunk).
- Map basemap (offline, bundled, ~5 MB in `ui/public/map/`, built by `npm run map-data` from Natural Earth public-domain data and Noto Sans glyphs): country shapes and names, state/province borders, lakes, rivers, and about 7,300 named cities that appear as you zoom in (using each feature's own `min_zoom`). Light and dark colors follow the theme. Max zoom 11 (the data is 1:10m scale). `world-atlas` was dropped because it breaks Russia, Fiji and Antarctica on a flat map. Checked in a headless Edge at several zooms in both themes.
- CSP gained `worker-src 'self' blob:` for MapLibre.
- Verified: 49 Rust tests (hash robustness, BK-tree against brute force, duplicate rules, full pipeline with a GPS photo and copies, API); migration 5 on a copy of the real library; `svelte-check` clean, build OK.
- Verified in the running app (`cargo tauri dev` on a scratch library of 44 real photos/videos from `I:\My Photos` plus 2 planted copies, driven through WebView2's debugging port with screenshots): all 46 items indexed with 0 errors; 13 places (Sodpur, Cherrapunji, Gangtok...) with covers; 41 map points; 2 exact duplicate groups; Map page draws India with clusters (32, 8) and a single dot, a cluster click zooms and splits it, and the grid follows the visible area; Duplicates "Trash the other" + Undo; place-name search (`sodpur`, `meghalaya`); search box; selecting photos and favoriting them with a toast.
- Bugs found and fixed by running it: (1) MapLibre 6 could not find its worker when bundled (fixed with `setWorkerUrl` and a Vite `?url` import; the production build ships the worker too); (2) MapLibre's own CSS collapsed the map to zero height (CSS specificity); (3) stale cluster markers stayed after zooming (now refreshed on `idle`); (4) a pending debounced search could pull you back to the search page after leaving it.

**Phase 5: TV (DLNA + /tv)**
- Network listener (`crates/server/src/lan/`): off by default; started, restarted or stopped whenever settings change (and at startup if it was on). Read-only router (none of the desktop API exists on it), trashed/missing items are 404, Host header must be an IP or this PC's name (blocks DNS rebinding), and open streams are cut when sharing is switched off. Port-in-use shows a clear error in Settings.
- Settings: `lanTvPage` and `lanDlna` switches (both on by default once sharing is on), device name, PIN (web page only), port, "convert videos". Hidden per-install values (`_dlnaUuid`, `_lanSecret`) via `settings::persistent_secret`.
- Video conversion (`photos_core::transcode`, `server/src/stream.rs`): FFmpeg to H.264/AAC, at most 1080p (portrait 1080x1920), rotation applied, keyframe every 2 s; MPEG-TS for DLNA, fragmented MP4 for browsers. Encoder found once (NVENC on this PC, then Intel/AMD, else x264); `-hwaccel auto` decoding (4K60 HEVC: 10.5 s down to 1.8 s for a 5.9 s clip). Seeking restarts FFmpeg (`TimeSeekRange.dlna.org` / `?t=`); a client's previous conversion is stopped when it starts a new one; at most 3 at once; FFmpeg is killed when the client disconnects.
- Migration 6: `media.video_codec` / `audio_codec`, stored by the metadata stage; videos indexed earlier are probed on first use. TV photo copies: upright JPEG within 3840x2160 (`cache/tv/`), small JPEG_TN thumbnails within 160x160 (`cache/tn/`).
- DLNA (`crates/server/src/dlna/`): SSDP on every adapter (shares UDP 1900 with Windows' SSDP service; alive at start and every 5 min, byebye on stop or app exit), device description with Samsung `sec:` caps and an icon, ContentDirectory Browse (metadata + children, paging, UpdateID), `X_GetFeatureList`, `X_SetBookmark`, ConnectionManager, SUBSCRIBE stubs. Tree: Timeline/Year/Month, Albums, Places/Country/City (country names built in), Favorites, Videos, Folders on disk. Videos list the original first when a TV likely plays it (H.264/HEVC in MP4/MOV/MKV/TS with AAC/MP3/AC3), else the converted stream first. Samsung `getMediaInfo.sec` duration header.
- TV page (`ui/tv/`, built to `ui/dist-tv/` by `vite.tv.config.ts`, embedded with rust-embed): plain TypeScript (15 KB) for old TV browsers (ES2017, no newer CSS). Home rows (Recent, Albums, Places, Favorites, Videos), grids that grow as you scroll, arrow-key spatial focus, OK/Back (Back = history back, so the TV's Back key works), viewer with slideshow (3/5/10/20 s), video with seek ±10 s, conversion fallback (also when a browser claims a codec but never plays it), PIN keypad. Works on phones too (tap/swipe, native video controls).
- Desktop: "TV and network sharing" card in Settings (switch, addresses, QR code from `/api/lan/qr.svg`, options, firewall hint). The desktop viewer now plays a converted copy when the webview can't play a video. DLNA says goodbye on app exit.
- Verified: 76 Rust tests (SOAP/DIDL/SSDP formats with Samsung-style requests, object ids, paging, read-only router, host check, PIN cookie + lockout, trash hidden, Range, time seek with a real HEVC/Opus MKV converted to TS and fMP4); svelte-check clean; production UI build OK. In the running app on a scratch library (25 HEIC, 6 JPEG, 4 HEVC incl. 4K60 and a rotated portrait clip): SSDP search answered from 192.168.0.100, browsing over the LAN address, photos/thumbnails/streams with DLNA headers, conversion upright and faster than real time, no FFmpeg left over; TV page in headless Edge at 1080p driven with arrow keys (home, place, viewer, slideshow, Back twice with focus restored), converted video + seek, PIN wrong/right/reload; Settings card; busy port; switching off.
- Bugs found by running it: rust-embed did not expand `$CARGO_MANIFEST_DIR` (TV page "not built"); 4K HEVC converted slower than real time (CPU decoding); TV page files cached for an hour (stale page after an update); OK key relied on the browser turning Enter into a click; digit keys never reached the PIN screen; Settings card layout.

**Phase 6: AI (faces + CLIP)**
- New crate `crates/ml` (`photos-ml`): ONNX Runtime through `ort` 2.0.0-rc.15 (statically linked; only `DirectML.dll` sits next to the exe), DirectML on the GPU with automatic CPU fallback (also mid-run if the GPU fails), ONNX Runtime telemetry switched off, model warnings silenced.
- Models (not in git, ~190 MB): YuNet + SFace (OpenCV Zoo, Apache-2.0), CLIP ViT-B/32 quantized image + text encoders (Xenova export). Looked for in `MEMORA_MODELS_DIR`, `models\` next to the exe, then `%LOCALAPPDATA%\Memora\models`. `scripts/fetch-models.ps1` downloads them for development.
- CLIP tokenizer written by hand (byte-level BPE from `tokenizer.json`; gives OpenAI's ids) instead of the heavy `tokenizers` crate.
- Faces: YuNet on the whole picture plus four overlapping quarters for pictures over 960 px (finds small faces); score ≥ 0.85 (animal faces score ≤ 0.8); faces ≥ 32 px; 5-point similarity alignment to the ArcFace template; SFace 128-d vectors. Square face pictures cached in `cache/face/`.
- Grouping (`core/people.rs`): a face joins an existing person if its average similarity to their faces is ≥ 0.45; the rest are grouped by average linkage (merge ≥ 0.50, at least 3 faces). DBSCAN was tried first and chained different people together; average linkage keeps groups clean (a few people come out split instead, fixed with Merge). Runs after every ~200 new faces and when the queue empties (~10 ms for 300 faces).
- The user always wins: merges and manual moves lock faces (`locked = 1`), "Not this person" keeps a face out (`locked = 2`); a changed file keeps names and corrections for faces found in the same place (IoU > 0.5).
- Content search (`core/semantic.rs`): CLIP vectors in memory, compared with (query vector − vector of a neutral text). Raw CLIP scores could not tell real words from nonsense; this can. Hits need ≥ 0.02 and ≥ half the best score: precise rather than complete.
- Pipeline: migration 7 (`persons`, `faces`, `clip_embeddings`), stage 4 = recognised. The worker starts only after indexing is done; 3 decode threads prepare pictures (SIMD resizing with `fast_image_resize`) while the models run: ~30 ms per photo on the RTX 4070. `aiPending` is in the library status events.
- PCs without a graphics card: DirectML also runs on integrated graphics (any DirectX 12 GPU: Intel UHD/Iris, AMD Radeon), so most PCs still use the GPU path. With no usable GPU, or if DirectML fails (also mid-run), the worker switches to the processor by itself and Settings says "Runs on the processor". Text for searches is always encoded on the processor (~10–20 ms). Model time per run on the Ryzen 7 5700X3D, GPU vs CPU: YuNet ~2 vs ~4 ms (up to 5 runs per photo), SFace ~1 vs ~6 ms per face, CLIP image ~6 vs ~20 ms. Estimated ~50–70 ms per photo on the CPU (3,000 photos in 3–5 min, 100,000 in 1.5–2 h); a 4-core laptop is several times slower, because the models get at most a quarter of the threads (1–4, plus 3 decode threads). Recognition can be switched off in Settings; everything else works without it.
- Search box text now also matches people's names and picture content (file names OR person name OR CLIP). Timeline takes `person`.
- API: `/api/people` (+ `/{id}` PATCH name/hidden/coverFace, `/{id}/merge`), `/api/faces/{id}/thumb`, `/api/faces/{id}/person`, `/api/media/{id}/faces`, `/api/ai`, `/api/ai/refresh`.
- UI: People page (show hidden), person page (name, merge, hide/show), People row on Explore, people chips in the viewer's info panel ("Not this person" with Undo), "People and smart search" card in Settings (on/off, GPU, progress, model folders), new search wording. Switch/checkbox styles moved to `app.css`.
- DLNA: People folder (named, not hidden people).
- Verified: 88 Rust tests (tokenizer ids, alignment, grouping incl. user locks, re-recognition keeping people, people API, DLNA People, worker end to end with the real models); clippy and svelte-check clean; UI build OK. In the running app on 247 real photos/videos: indexing + recognition in ~40 s on the GPU, 21 people, grouping checked by eye on contact sheets; search checked by eye ("dog", "food", "cake", "night sky", "selfie" right; nonsense finds nothing); naming, name search, "Not this person" + Undo, GPU/CPU and on/off switches; a photo dropped into the folder was recognised within 3 s.

**Phase 7 and v0.2 (2026-11-10)**
- RAW (`core/raw.rs`, new `Format::Raw`, 17 extensions): the largest embedded JPEG preview is shown (TIFF IFDs and sub-IFDs, `JPEGInterchangeFormat`, single-strip JPEG, RW2 tag 0x2E; files that aren't TIFF, like CR3/RAF, or whose preview sits in maker notes, like ORF, are scanned for embedded JPEGs; lossless sensor JPEGs are skipped). DNGs with no preview (the OnePlus 12R writes some) get a half-size development of uncompressed 8/16-bit Bayer data: black/white level, `AsShotNeutral` white balance, `ColorMatrix` (dcraw-style), automatic brightness, sRGB gamma. EXIF from the first 4 MB of TIFF-based files, else the preview's EXIF, or CR3's `CMT1/2/4` blocks. RAW photos get a display copy like HEIC; thumbnails, TV photos and recognition use it. Checked by eye on the 7 DNGs in `I:\My Photos` (one developed from sensor data, one rotated).
- HEIC rotation: checked on real iPhone 12 Pro portraits (`irot` 3 + EXIF 6, `irot` 1 + EXIF 8), an iPad and a Samsung A32; all upright. FFmpeg 7.1 does not report `irot`, so `core/heif.rs` reads `ispe`/`irot`/`imir` itself (transformations composed in order; `irot` wins over EXIF, EXIF used when there is none).
- Video dates (`metadata::video_time`): `creation_time` is read as UTC, unless only local time lines up with the file's modification time (cameras that write local time: the file is written while recording, so it was modified within the clip's length of the start). The offset comes from the GPS position's time zone (`tzf-rs` boundaries + `chrono-tz`, offline), else the PC's zone (not stored). Photos with GPS but no EXIF offset get one too. Migration 8 sets a flag; the library then re-reads the dates of indexed videos once in the background (only `taken_at`/`tz_offset`). On the real library: 337 videos gained +05:30, 91 without GPS unchanged.
- HDR: PQ and HLG videos (`color_transfer`) are tone mapped in converted streams and thumbnails: `zscale` to linear with SDR white at 203 nits (BT.2408), BT.709 primaries, Möbius curve, 8-bit 4:2:0. A test colour comes back within a few levels (196,64,31 for 200,60,40; without tone mapping 108,73,48). 4K60 HDR converts at about real time on this PC (6.2 s for 6 s with NVENC; 3.1 s without tone mapping), 4K30 and 1080p with room to spare. The HDR kind is noted from the metadata probe, so thumbnails need no extra probe.
- Faster indexing (sample of 252 real files: 160 HEIC, 60 JPEG, 25 MP4, 7 DNG): 30.0 s -> 15.1 s overall, grid filled 5.3 s -> 0.5 s. What did it: (1) HEIC size/rotation from the boxes instead of ffprobe (~100 ms -> <1 ms each); (2) the HEIC tile layout also comes from the boxes, mirroring FFmpeg 7.1's stream numbering (one stream per `hvc1`/`av01` item in `iinf` order), so each HEIC is opened in FFmpeg once instead of twice. Opening a 48-tile HEIC costs FFmpeg ~200 MB and a thread per tile, which is why parallel HEIC decoding tops out at ~14/s. Checked identical to ffprobe on all 2,264 HEICs in `I:\My Photos` plus the samples (`heif_layout_matches_ffmpeg` test); unusual files (overlays, `clap`, image sequences) still ask FFmpeg; (3) `jpeg-encoder` (SIMD) instead of the `image` crate's JPEG encoder (2560 px copy 107 -> 45 ms); (4) workers pull items one at a time from a queue instead of 32-item batches that waited for their slowest file; (5) up to 8 workers (all logical cores but two) at below-normal priority, and FFmpeg processes started by them too (`ffmpeg::mark_background_thread`); recognition threads too. More than 8 workers gained ~3% for a lot of memory.
- Installer (`scripts/build-installer.ps1`): NSIS only, per-machine (UAC once). Bundles FFmpeg 7.1.1 essentials (Gyan, GPLv3, pinned by SHA-256, re-extracted each build), the models, `DirectML.dll` and license files (`THIRD-PARTY-NOTICES.md`). Tauri drops the drive letter of absolute resource paths on another drive, so staged files (in the target folder on C:) are reached through a junction `src-tauri/installer-files`. ort's `DirectML.dll` link in `target/release` is replaced by a copy (else the build writes into ort's cache). Installer ~208 MB.
- Firewall (`src-tauri/installer-hooks.nsh`): one inbound rule for memora.exe, private and domain profiles only, added on install (after removing an old one) and removed on uninstall. Settings explains that the home network must be set to Private.
- Signing (`scripts/sign.ps1`): certificate thumbprint (store, tokens), .pfx, or Azure Trusted Signing; DigiCert timestamp. Tested end to end with a throwaway self-signed certificate: memora.exe, the uninstaller, NSIS plugins, ffmpeg/ffprobe and the installer get signed; DirectML.dll keeps Microsoft's signature. `MEMORA_REQUIRE_SIGNING=1` fails a build without a certificate.
- Verified: 102 Rust tests (RAW parsing and development, HEIF boxes and layout, video time zones, HDR thumbnails and conversion, RAW end to end); clippy and svelte-check clean.

**v0.3 step 1: Custom title bar** (2026-11-11)
- Window has no Windows title bar (`decorations: false`, `shadow: true`, so the Windows 11 shadow and rounded corners stay). The top bar is the title bar, now 48 px (search box 36 px). Its empty parts and the viewer's top row are drag areas (`data-tauri-drag-region="deep"`: links, buttons and fields inside still work); double-click maximizes/restores.
- `components/WindowControls.svelte`: minimize, maximize ↔ restore, close (Segoe Fluent Icons glyphs, close hover `#C42B1C`). One copy, `position: fixed` at the top right with z-index 1000, so it stays above the viewer, dialogs and toasts. White glyphs over the black viewer, theme colours when the viewer's info panel is under them. Dimmed while the window is inactive (`lib/window.svelte.ts` follows maximized/focused). Hidden in the dev browser (`--wc-w` is 0 there).
- Window menu: right-click on an empty part of the bar or on the buttons, and Alt+Space, open the Windows window menu (`show_system_menu`, `GetSystemMenu` + `TrackPopupMenu`; Restore/Move/Size/Maximize are greyed or enabled by hand since Windows only does that for native captions).
- Snap Layouts work: a small native child window (`window_chrome.rs`) sits over the page's maximize button and answers `WM_NCHITTEST` with `HTMAXBUTTON`, so Windows 11 shows the layout picker on hover. It also does the click (maximize/restore), right-click (window menu) and sends hover/press to the page (`max-button` event), which draws them; the page reports the button's position (`set_max_button_rect`) and the child follows on resize/DPI change. It never paints, and the page shows through it. (A layered child, `WS_EX_LAYERED`, fails with "handle is invalid": that needs a Windows 8+ manifest entry; not needed.)
- Resizing from the edges is Tauri's own (a transparent strip over the top edge; the other edges use the invisible Windows 11 borders). Size, position and maximized state are kept between launches (`tauri-plugin-window-state`; the window starts hidden and is shown after the state is restored).
- Sidebar bottom shows the library: "2,629 photos · 427 videos" and "76.8 GB on disk" (new `photos`, `videos`, `bytes` in the library status, counted like `total`: indexed, not trashed; tested in `indexes_new_folder_end_to_end`); it links to Settings. Header layout unchanged (other layouts were tried on 2026-11-11 and the user kept this one). Archive (step 3) must leave archived items out of these counts too.
- Checked in `cargo tauri dev` (by script: screenshots, clicks): buttons, maximize fits the work area (no overhang), double-click on the bar, drag by the bar and by the viewer row, resize from the right and top edges, window menu with the right items greyed, Snap Layouts picker on hover, hover cleared after maximize/restore, viewer with and without info panel, Close shuts down as before (LAN sharing stopped), relaunch comes back maximized. Not checked: Alt+Space (PowerToys Run takes it on this PC), a second monitor with another scaling, light theme, Windows 10.

**v0.3 step 2: Shared actions and right-click menus** (2026-11-11)
- `lib/actions.ts`: favorite, add to album, remove from album, album cover, trash, restore, delete forever, Show in Explorer, open with default app, copy file path(s), copy image, set as desktop background; each takes an `ActionContext` (items, scope, album, callbacks such as `onremoved`) and shows the same toast (with Undo where it applies). The selection bar (`MediaPage`), the viewer's toolbar and keys, and the menus all call these; the old per-component copies are gone.
- `components/ContextMenu.svelte` + `lib/contextMenu.svelte.ts`: one menu at a time, opened at the pointer and moved to stay inside the window; Shift+F10 / Menu key open it at the focused tile with the first entry focused; arrows, Home/End, Enter, Esc/Tab (focus goes back); closes on a click elsewhere, scroll, wheel, resize, window blur and route change. Above the viewer and dialogs, below the window buttons.
- Menus: photo tile (Open, favorite, Add to album…, album cover/remove on album pages, Copy image, Copy file path, Show in Explorer, Open with default app, Set as desktop background, Info, Select, Move to trash); video tile (Play, no Copy image/wallpaper, "Open with default player"); selected tiles ("N items selected", favorite, Add to album…, Copy file paths, Move to trash); a tile outside the selection replaces the selection (like Explorer); viewer (same as the tile, without Open/Select; Info toggles the panel); Trash (Open, Restore, Delete forever, Show in Explorer, Copy file path). Info on a tile opens the viewer with the info panel (`Viewer` `info` prop).
- The webview's own menu (Back, Reload, Save image as, Inspect…) is off everywhere except text fields and selected text (Cut/Copy/Paste stay).
- Copy image is done in the page (no clipboard plugin): the viewer's JPEG (upright; HEIC/RAW via their display copy) is drawn to a canvas and written as PNG with `navigator.clipboard.write`, started inside the click. Copy file paths uses the new `POST /api/media/paths` (paths in the order asked, unknown ids skipped; tested in `organize_favorites_albums_trash_and_search`).
- `set_wallpaper(id)` (Tauri command, `desktop.rs`): copies the TV picture (`thumbs::ensure_tv`: upright JPEG ≤ 4K, display copy for HEIC/RAW) to `<data>\wallpaper.jpg` and calls `SystemParametersInfoW(SPI_SETDESKWALLPAPER)`; Windows keeps its own fit setting.
- Checked in the running app over the WebView2 DevTools port (`WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333`, no real mouse): all menus above and their entries, Copy file path (clipboard has the path), Copy image (962×1280 image on the Windows clipboard), favorite on/off from the menu, selection header and "outside the selection", keyboard-opened menu + arrows + focus return, Info from the grid and the viewer, Move to trash → Trash menu → Restore, native menu blocked on the sidebar but not in the search box. Not run: Set as desktop background (it would have changed the desktop of the PC in use), Delete forever, Show in Explorer and Open with default app from the menu (same commands as before, unchanged).
- Not done from the plan's table: "Remove from library" for missing/broken tiles (missing files are not listed in any grid, and a tile whose thumbnail fails still has the normal menu); Archive entries come with step 3; album/person/place card menus stay "later".

**Custom video player** (2026-11-11, asked for between v0.3 steps 2 and 3; demo approved first)
- `components/VideoPlayer.svelte` replaces `<video controls>` in the viewer (made per item). Seek bar (buffered range, hover fill, drag to scrub; originals follow the pointer, converted videos jump on release), hover preview from a storyboard, play/pause, ±10 s, volume (mute + slider on hover), time (click: time left), "Converted" chip, speed badge, Copy this frame, settings menu (speed 0.25×–3×, Loop, Keyboard shortcuts), full screen. Click = play/pause, double-click = full screen, touch tap = show/hide controls. Centre flash, on-screen messages ("+10 s", "Volume 80%"), spinner, replay button at the end.
- Auto-hide: after 2.5 s of playing with the mouse still, the controls, the viewer's bar and arrows, the window buttons and the cursor fade (`idle` bound to the viewer); not while paused, a menu is open, scrubbing or the pointer rests on the controls.
- Keys (capture phase, so Esc closes the player's menu/sheet before the viewer): Space/K, J/L ±10 s, `,`/`.` one frame (1/30 s), 0–9 jump, Home/End, ↑/↓ volume, M, `[`/`]` speed, F, `?` shortcuts sheet. ←/→ still go to the previous/next item; on a focused seek bar they skip 5 s. Off while a dialog, picker, right-click menu or text field has the keys.
- Full screen: `lib/window.svelte.ts` `setFullscreen` (Tauri `setFullscreen`, new permission `core:window:allow-set-fullscreen`; the browser's API in dev). `:root.fullscreen` hides the window buttons. Esc leaves full screen first; closing the viewer leaves it too.
- Converted videos (webview can't decode the original): the player continues at the failed time with `stream.mp4?t=`, keeps the offset so the bar shows file time (length from the library), and every jump starts a new stream (`&r=` forces one for loop/replay; the server stops the previous FFmpeg). The old "skipping ahead is limited" note is gone. If the converted copy fails too: the "can't play" message with "Open in default player".
- Storyboard (`GET /api/media/{id}/storyboard`, `thumbs::ensure_storyboard`, cache kind `story`): up to 100 frames (one per second for shorter videos), 10 per row, tiles fitting 240×240, made with keyframes only (`-skip_frame nokey`, `-hwaccel auto`, HDR tone mapped, 5 min limit): 0.7 s for an 81 s 1080p60 HEVC phone video. Layout in the `X-Storyboard` header (`frames=…;interval=…;cols=…;rows=…`, exposed through CORS); the UI loads it 0.6 s after opening.
- Copy this frame: `GET /api/media/{id}/frame?t=` decodes that exact frame at full size (rotation, HDR tone mapping) as PNG; the clipboard write starts inside the click.
- Remembered: volume and mute across launches (`localStorage` `memora.video`); speed, loop and "time left" for the session.
- Tests: `storyboard_layout`, `storyboard_and_frame_of_a_video` (core), `video_storyboard_and_frame` (server: header, CORS expose, sizes, PNG). photos-core (71) and photos-server (27) tests pass; clippy and svelte-check clean.
- Checked in the running app over DevTools: plays originals (HEVC portrait and landscape), hover preview at 1:40 of a 4:47 video, click-to-seek, all keys above, menu, shortcuts sheet, Esc order, idle fade and wake, full screen on/off and on closing, Copy this frame (PNG on the clipboard), converted fallback (forced by breaking the original URL) continuing at 0:01, jump to 2:52 and J/L restarting FFmpeg with the right time, and the "can't play" message.
- Not checked: a real unsupported file (every video on this PC plays natively), touch input, the frame step on non-30 fps video (steps 1/30 s).

**v0.3 step 3: Archive** (2026-11-11)
- Migration 9: `media.archived_at` (time archived); the partial timeline/favorites indexes now leave archived items out; new `media_archived` index.
- One condition for "shown in the everyday lists": `media::VISIBLE` / `VISIBLE_M` (`missing = 0 AND trashed_at IS NULL AND archived_at IS NULL`), used by every listing query: timeline scopes, browse (DLNA years/months/folders), albums (counts, covers), people (counts, covers), places, map points, cameras, duplicates and the library counts (sidebar totals). Search by people and picture content goes through the same timeline filter, so archived items aren't found either.
- `Scope::Archive` (archived, not trashed). Trash wins: the Trash page shows trashed items whether archived or not, and Restore puts an archived item back in the Archive. Albums, favorite flag and faces are kept, so Unarchive returns the item exactly where it was (album cover included). Recognition and hashing still run on archived items.
- Core `organize::archive` / `unarchive`; API `POST /api/media/archive|unarchive`, `/api/timeline?scope=archive`, `archived` (and `trashed`) in media details. The server's trash/restore/archive/unarchive handlers share one helper.
- Network: setting `lanArchive` (on by default; "Show the Archive" in the sharing card, with the no-PIN warning for DLNA). TV page: Archive row (only when shared and not empty) + `items?scope=archive` (404 when off). DLNA: top-level Archive folder `r` (only when shared and not empty), listed newest first and paged. `media::get_visible(db, id, archive)`: archived items are served (files, details) only while the Archive is shared; DLNA item metadata only under the Archive folder.
- UI: Archive page in the sidebar under Favorites (`MediaPage scope=archive`, empty-state text), Archive/Unarchive in the selection bar, viewer toolbar and right-click menus (Shift+A everywhere, toast with Undo), "Archived" row in the info panel.
- Tests: `archived_items_show_only_in_the_archive` (core: timeline, favorites, album + cover fallback, text and people/content search hits, places, map, cameras, years, network visibility, trash + restore back to Archive, unarchive), the API flow in `organize_favorites_albums_trash_and_search`, `archive_row_and_folder` (LAN: TV home row and list, DLNA root/folder/metadata/folders on disk, files, switching `lanArchive` off). photos-core 72, photos-server 28 tests pass; clippy and svelte-check clean.
- Checked in the running app (copy of the real library, 3,061 items; migration 9 ran on it) over DevTools: right-click menu entry, Archive from the menu (toast, sidebar count 2,629 → 2,628, item on the Archive page), viewer on the Archive page (Unarchive button, "Archived" info row, Shift+A unarchives and closes on the last item), Shift+A on a 2-item selection in Photos + Undo from the Archive page, the "Show the Archive" switch in Settings. The TV page row and DLNA folder were tested by the LAN tests only (no TV run).

**v0.3 step 4: Move the data folder, locate a moved folder** (2026-11-11)
- `core/storage.rs`: the home folder (`%LOCALAPPDATA%\Memora`, or `PHOTOS_DATA_DIR`) always exists and holds `location.json` (`dataDir`, `removeOld`) when the data was moved. `AppPaths` gained `home_dir`; `from_env` returns `Err(dir)` when the data is on a drive that isn't connected, and the app then asks "Retry / Cancel" (Win32 `MessageBoxW`, since the dialog plugin can't block during setup). Models are looked for in the home folder (they don't move with the data).
- Target rules (`storage::target_for`, plus `desktop::free_space` in the app): no UNC paths or mapped network drives (`GetDriveTypeW`), no OneDrive (also `%OneDrive*%`), Dropbox, Google Drive or iCloud folders, not inside the current data folder; a non-empty folder gets a `Memora` folder inside it, which must be empty and not hold another library; picking the home folder moves the data back there (and `location.json` goes away). Needs the data size + 500 MB free.
- Move (`data_folder.rs`, Tauri commands `data_folder`, `data_folder_target`, `move_data_folder`, `restart_app`): copy the cache while the app keeps working (`data-move` progress events), then stop indexing, recognition and TV sharing, copy the rest of the cache, `VACUUM INTO` the new folder (a consistent copy), copy `wallpaper.jpg`, check the copy (`quick_check`, same item count), write `location.json` and restart. On the next start `storage::finish_move` deletes the old database, cache and our files (nothing else; the folder itself only if it is then empty and isn't the home folder). A failure before the switch removes the partial copy; if background work was already stopped, the UI offers "Restart Memora".
- Locate folder (`folders::relocate`, `Library::locate_folder`, `POST /api/folders/{id}/locate`): refuses folders overlapping another watched folder, and folders where none of a sample of up to 200 indexed files are; rewrites `folders.path` and every item's path prefix in one transaction (ids stay, so favorites, albums, people and thumbnails stay), re-watches and rescans the folder.
- UI: Storage card in Settings (data folder link, size, Move… with confirm and a progress dialog, the network/cloud note); "Locate…" next to "Not available" folders. The data folder line left the About card.
- Tests: `move_round_trip` (copy, second pass picks up a new thumbnail, switch, reopen, old copy removed but other files kept, and back home), `unreachable_data_folder`, `refuses_bad_targets`, `relocate_rewrites_paths` (core), `locate_a_moved_folder` (server: folder renamed on disk, wrong folder refused, same item with its favorite after locating). photos-core 76, photos-server 29 tests pass; clippy and svelte-check clean.
- Checked in the running app (scratch copy of the real library): Storage card and Locate… button shown; locating a planted "moved" folder (wrong folder refused, right one rewrote the path, the favorite stayed, rescan picked the file up); `data_folder_target` refusing a OneDrive and a network path; moving 30 MB of data to another folder (app restarted on it with all 3,057 items, old copy deleted, `location.json` written) and back home (`location.json` and the other folder removed). The native folder picker and the progress dialog were not clicked (the picker can't be driven over DevTools; the moves were started with the same commands). In `cargo tauri dev` the restart ends the dev session and Vite with it, so the restarted window can't reload the page there; installed builds load the page from the app.
- Not done: "Use the library already there" when the picked folder has a library; a move between PCs is C2's job.

**v0.3 step 5: `.memora` library info (C2a)** (2026-11-11)
- `core/sync.rs`. Layout as planned, minus `snapshot\` (this PC's log is rewritten in place instead, see compaction): `.memora\memora.json` (format, folder id, created by/when, a note that it is safe to delete) and `changes\<device>.jsonl`. The folder is made hidden (`attrib +h`); the scanner already skips dot-folders.
- Log lines: a header `{"memora":1,"device","name","gen"}`, then `{"c":clock,"f":field,"p":item,"s":sub,"h":hash,"v":value}`. Fields: `fav`, `arch`, `trash` (time or null), `album` (`{name, created}`, null = deleted), `album_item`, `album_cover` (`{p, h}`), `person` (`{name, hidden}`), `face` (box `x,y,w,h` → `{person: uid, locked}`). `p` is the path relative to the folder holding `.memora`, lowercase with `/`.
- Clock: `<unix ms, 13 digits>.<counter, 8 digits>.<device>`, compares as text. One process-wide hybrid logical clock; every line read moves it forward, so a PC whose clock runs behind still writes later changes after one that runs ahead. Device id: random, kept in settings `_syncDevice` with the computer name; a database copied to another PC gets a new id.
- Migration 10: `albums.uid`, `persons.uid` (filled, unique, set by a trigger on insert, so no code that creates albums or people changed); `folders.sync`, `folders.sync_root`; `sync_reg` (newest known value per folder + field + item + sub, with an `own` flag for "belongs in this PC's log"), `sync_outbox`, `sync_cursor` (file, generation, byte offset).
- Recording: favorite/archive/trash (`organize`), album rename/delete/cover/add/remove (`albums`), person rename/hide/merge and face moves / "Not this person" (`people`) now run in one transaction with `sync::record*`, which updates `sync_reg` and queues the line in `sync_outbox`. Only for folders with `sync = 1`; nothing changes for the others. Album and person values go to every synced folder that has their items (copied with their original clock when a folder first needs them, so copying never wins over a newer rename). Naming or hiding a person also records which faces are theirs, so other PCs can find them.
- Writing (`flush`): appends queued lines to this PC's log, `sync_data`, then drops them from the outbox. A folder that is unplugged or read-only keeps its lines queued. A missing log is written whole (header + this PC's values).
- Reading (`read`): every `*.jsonl` (this PC's too: harmless, and it restores a re-added folder), from the stored offset when the generation matches, else from the start; only complete lines, so a half-synced last line waits. Values are stored if their clock is newer.
- Applying (`apply`): makes the library match the newest value per field for every synced folder, in one `IMMEDIATE` transaction; changes only what differs and returns whether anything did. Items: the one at the same relative path (files still on disk), else every copy with the same hash (renamed files). Albums are created, renamed and deleted by uid; album items and covers follow. People: name and hidden by uid. Faces are matched by box (IoU > 0.5): a face the user set elsewhere is set the same; for a face grouped automatically there, an unnamed group this PC made on its own is folded into that person, so the name covers PC 2's own grouping. `ai::save` applies an item's recorded faces in the same transaction, so names are there before grouping runs.
- Switching on (`enable`): uses a `.memora` in the folder or a parent (`find_root`; a subfolder of a synced folder shares it), else creates one. What the folder's items already have is recorded with clock 0: it fills gaps but never overrides what other PCs recorded. Switching off (`disable`) keeps `.memora` and forgets this PC's copy of its values; "Remove" also moves `.memora` to the Recycle Bin (only a `.memora` in the folder itself). Locate folder finds the `.memora` again.
- Compaction (`compact`, at start): this PC's log is rewritten (temp file + rename, new generation) when it has more than 3× the lines it still needs; other PCs see the new generation and read it again.
- Library: a `library-sync` thread writes changes within a moment of an edit (`notify_changed` kicks it), reads when the watcher reports a change under `.memora` (those events no longer go to the scanner) and every 30 s (network drives may not report changes), and applies after scans, after hashing settles and after recognition.
- API: `POST /api/folders` takes `keepInfo`; `POST /api/folders/probe` (`{root, pcs}` before adding); `PATCH /api/folders/{id}` `{keepInfo}`; `DELETE /api/folders/{id}/info`; `POST /api/library/sync`; folders carry `sync`, `syncRoot` and `libraryInfo` (`root`, `writable`, `otherPcs`, `unsent`).
- UI (Settings → Library folders): adding a folder asks "Keep library info in this folder?" (or "Library info found from <PCs>, use it?"); a badge "Info in folder" / "Shared with N PCs" (tooltip: where, which PCs, unsent changes) or "Info kept on this PC" when it can't be written; a sync button per folder switches it on/off, with the Recycle Bin option when switching off.
- Tests (`sync/tests.rs`, two databases with copies of the photos at different paths and logs carried between them): favorites, archive, trash/restore, album create/rename/remove/cover/delete both ways; concurrent renames (later wins on both); half-written last line; renamed file found by hash; existing info fills gaps without overriding; subfolder using its parent's `.memora`; unplugged drive keeps changes and writes them later; a PC whose clock runs a year ahead; compaction and re-reading a new generation; people names, hiding and "Not this person" onto PC 2's own grouping; a name applied when recognition finds the face later. Server: `library_info_in_the_folder`. photos-core 84 (+1 ignored `real_library_upgrade`), photos-server 30, photos-ml 7 tests pass; clippy and svelte-check clean.
- Checked on a copy of the real library (not in the running app): migration 10 ran (3,061 items, 59 people got uids), recording the first folder's state took 108 ms (832 values) and applying it changed nothing.
- Not done yet: the "Empty trash" dialog does not yet say it applies to the other PCs; a second real PC, a NAS and a cloud-synced folder were not tried; the Settings controls were not clicked in the running app.
- Not synced (as planned): person cover choice, album cover for albums whose cover is in a folder that isn't synced, the folder list and per-PC settings.

**v0.3 step 6: `.memora` index snapshot (C2b)** (2026-11-11)
- `core/snapshot.rs`: `.memora\index\<device>.db`, a plain SQLite file: `meta` (format, `PIPELINE` version, recognition models `ai::RECOGNITION`, device, app version, created), `items` (relative path, size, mtime, blake3, kind, mime, size in pixels, duration, orientation, date and offset, camera, GPS, phash, place names, codecs, recognised, CLIP vector) and `faces` (box, score, vector, person uid, locked). Only items that finished indexing without an error.
- Writing: a temp file renamed over the old one, so other PCs only ever see a complete file. The sync thread writes a folder's snapshot when its index changed since the last one (`folders.index_mark`, a cheap summary of stages, times, ids and face assignments) and nothing in it waits for indexing: checked 5 minutes after the last check once recognition is done, every 30 minutes while it is still running. "Sync now" (`POST /api/library/sync`) writes too.
- Reading: every `*.db` in `index\` that has the same format and `PIPELINE` (others, and damaged files, are skipped), newest first, opened read-only and immutable (no lock or journal files on a NAS or synced folder). After a scan adds items to a synced folder, `snapshot::import` copies everything for new items with the same relative path, size and modification time and sets them to stage 1 with `media.imported` = 1, or 2 when the snapshot's recognition models match (faces with their person uid, so names from the change logs fit, and the CLIP vector; the in-memory content-search index is reloaded). The thumbnail stage then moves imported items straight to stage 3 (hashed) or 4 (recognised): only thumbnails are made on PC 2. A changed file (`Op::Reset`) loses `imported` and is processed normally.
- Migration 11: `folders.keep_index` (on by default), `folders.index_mark`, `media.imported`.
- API and UI: `PATCH /api/folders/{id}` takes `keepIndex` (switching off deletes this PC's snapshot); `libraryInfo.indexBytes`. Settings shows "Also keep the finished index there, so other PCs skip indexing (12 MB)" under a synced folder.
- `photos-ml` test `names_the_model_files` keeps `ai::RECOGNITION` in step with the model files.
- Tests: `second_pc_takes_over_unchanged_files` (metadata, place, faces with the same person uid, CLIP; a changed file is indexed; a snapshot from other recognition models brings everything but faces; switching off deletes it), `snapshots_from_other_versions_are_not_used` (old `PIPELINE`, damaged file), `second_library_skips_indexing_end_to_end` (two running libraries: PC 2 ends with the same hashes, every item imported and at stage 3, thumbnails made). Server: `library_info_in_the_folder` covers `keepIndex`. photos-core 87, photos-ml 8, photos-server 30 tests pass; clippy and svelte-check clean. Migration 11 ran on a copy of the real library.
- Not done: matching by hash when a copy changed the files' modification times (those files are indexed normally); measuring the snapshot size and write time on the real library; a run in the app with a second PC.

**v0.3 step 7: library info backup file (C3), and fixes** (2026-11-18)
- `core/backup.rs`. Built as one JSON file (`.memora-backup`) instead of the planned zip of `.memora` folders: it covers every watched folder, synced or not, needs no zip library, and is readable. Contents: `memora: "memora-backup"`, format, created, app version, PC name; settings; folders (path, keep info, keep index); albums (uid, name, created, cover); people (uid, name, hidden); items with any user state (folder index, path relative to that folder, hash; favorite, archived, trashed, album uids, faces: box, person uid, locked). Faces kept: decided by the user, or of a person named or hidden. Not in it: the index, thumbnails, the vault (later).
- Writing: temp file + rename. Reading refuses files that aren't backups, and backups from a newer format.
- Restore (`Library::restore_backup`): folders of the backup that exist on this PC but aren't watched are added (with their keep-info / keep-index choice, in the same write as the folder) and walked right away, so their items can be found; then `backup::restore`. Items are found at the same relative path in the folder with the same path, else by hash anywhere in the library. It adds and never undoes: favorites set, archived and trashed items put back (with today's time, so the 30-day trash clock starts again), albums made with the backup's uid/name/date or topped up when the uid exists (names given since stay; a cover only if the album has none), people named only where they have no name. Faces go through `sync::apply_face` (same matching and "fold an unnamed group into the person" rule as `.memora`). Faces of items recognition hasn't reached wait in `restore_faces` / `restore_persons` (migration 12) and are applied in `ai::save`. Everything goes through the usual recording functions, so synced folders get it in `.memora` too. Settings only if asked (unknown keys skipped); network sharing and recognition pick them up.
- API: `POST /api/backup` `{path}` (writes, returns a summary), `POST /api/backup/read` `{path}` (summary for the confirm dialog), `POST /api/backup/restore` `{path, settings}` (returns what it did). UI: "Back up library info" card in Settings (Back up… with the save dialog and a dated default name, Restore… with a summary to confirm, then "Also restore the settings?", and a line saying what was restored, what waits for recognition and what wasn't found). New capability `dialog:allow-save`.
- Empty trash: the confirm text says the trash is shared with other PCs when a folder keeps library info.
- Indexing progress: the top bar and Settings show the current step ("Reading files… / Making thumbnails… / Finishing up… N left", `Status.toRead/toThumbnail/toHash`). The overall count only fell in the last step, so it looked stuck for most of a big first indexing.
- Race fixes found by the parallel test runs: a folder added with library info is now written with it in one insert (`folders::add_with`), so the startup scan can't see it without and skip the index snapshot; the scan thread updates the counts before it reports "scanning done", so the library never looks idle with work left.
- Tests: `everything_comes_back_on_a_fresh_install` (fresh library: folder added back, favorite, archive, trash, album with cover, settings; faces named and "not this person" applied when recognition finds them; nothing left waiting), `restoring_adds_but_never_undoes`, `items_are_found_by_hash_when_the_folder_moved`, `rejects_files_that_are_not_backups`; server `backup_and_restore`. photos-core 91, photos-ml 8, photos-server 31 tests pass (core 5 times in a row); clippy and svelte-check clean. Migration 12 ran on a copy of the real library.
- Not checked in the running app: the Back up / Restore buttons and dialogs.

**v0.4: Locked folder (vault)** (2026-11-18)
- `core/vault/crypto.rs`: master key wrapped by Argon2id(password) and by the recovery key (160-bit, Crockford base32, `blake3::derive_key`); records sealed with XChaCha20-Poly1305 (record id as associated data); files in 64 KiB chunks, nonce = random prefix + chunk number, associated data = header + chunk number + last-chunk flag, so swapped, cut, reordered or mixed-in chunks fail. `Decryptor` is `Read + Seek` (HTTP Range).
- `core/vault/store.rs`: `<data>\vault\vault.db` (hidden folder) with `meta` (only key-wrapping data) and `items(id, sealed record)`: no plaintext columns, `secure_delete`; every file under its own random name in `files\ab\`. Setup, unlock, recover (recovery key + new password), change password (needs the old one), new recovery key, lock, auto-lock (`lock_if_idle`), list/get/open part, add (encrypt, then decrypt and compare the hash), move out (decrypt to a folder under a unique name with the old modification time, hash check, then delete), delete, favorite. Stray files from an interrupted move are removed on unlock.
- `core/vault.rs` `move_in`: encrypt original + cached thumbnail + display copy, check, **delete the original** (if it can't be deleted, the vault copy is dropped and the item stays in the library; changed from the plan's "row first", so a locked file can't end up in both places), then purge: `sync::forget_items` (this PC's `.memora` values for the item, album-cover values pointing at it, queued lines; the folder is marked `log_dirty` so its log is rewritten), rows (FTS, albums, faces and CLIP cascade) with `secure_delete`, cached pictures and face crops; then `VACUUM` + `wal_checkpoint(TRUNCATE)` so the name isn't left in free pages. `Library::move_to_vault` also rewrites the `.memora` log and index snapshot right away and resets the CLIP index. Migration 13: `folders.log_dirty`, FTS5 `secure-delete`. Moving the data folder copies `vault\` and deletes the old one.
- Server (`server/src/vault.rs`, localhost only): `/api/vault`, `setup`, `unlock`, `recover`, `lock`, `password`, `recovery-key`, `items`, `items/{id}[/thumb|display|original]` (decrypted on the fly with Range, 206/416), `move-in`, `move-out`, `delete`, `favorite`. Every answer is `Cache-Control: no-store`; 423 while locked, 403 for a wrong password. Auto-lock task (setting `vaultLockMinutes`, 1-60, default 5).
- App (`src-tauri/src/vault_guard.rs`): locks when Windows locks (`WTSRegisterSessionNotification`, `WM_WTSSESSION_CHANGE`) and, with `vaultLockOnMinimize`, on minimize; `clear_web_cache` empties WebView2's disk cache (`ICoreWebView2Profile2::ClearBrowsingData`, disk cache only) after a move in.
- UI: Locked folder page (Utilities tile): set up with the password typed twice, recovery key shown once with an "I've saved it" check, unlock, "Forgot password?" with the recovery key; grid with selection (favorite, Move out… with a folder picker, Delete forever), Lock button, notices auto-lock. `VaultViewer` (photos; videos through `VideoPlayer`'s new `source` prop, without conversion, storyboard or frame copy). "Move to locked folder" in the right-click menu and the selection bar (`actions.moveToVault`: unlock prompt, then a confirmation saying originals are deleted). Locked folder card in Settings. `PhotoGrid` takes a `thumbUrl`; prompts can be password fields.
- Tests: crypto (round trips at chunk edges, any range, damaged/cut/swapped/mixed chunks, wrong key, recovery key format), store (lifecycle, nothing readable on disk, move out, swapped records, auto-lock), `moved_items_leave_no_trace_in_the_library` (original gone, album cover falls back, search, caches, and the name and hash are in no file under `.memora` or the data folder), `an_original_that_cant_be_deleted_stays_in_the_library`, server `locked_folder`.
- Checked in the running app (scratch library: 176 real photos/videos copied from `I:\My Photos` plus 4 generated, `PHOTOS_DATA_DIR`): setup screens, recovery key, right-click → Move to locked folder → confirm (file gone, library 180 → 179, its memory 4 → 3, the file name found in no file on disk), a 1 GB 4K HEVC video moved in 3.4 s and played with seeking in the vault viewer, Lock → thumbnails answer 423 and the unlock form shows, wrong-password message, unlock, move out (file back with its old date, indexed again). Not tried: locking on Windows lock and on minimize, the Change password / New recovery key buttons (covered by tests), the effect of clearing the WebView cache (the command ran without error).
- Changed after use (2026-11-18): locked items stay in their photos folder. Each watched folder keeps the items that came from it in `<folder>\.memoraault\` (`vault.db` + `files\`; the outermost watched folder holding the original); items from outside every watched folder stay in `<data folder>ault\`. One master key for all places: each `vault.db` holds the same key data plus a `key_id` (derived from the master key) and `changed`; unlock tries the newest key data of each key, and places with an older copy (password changed while their drive was away) get the current one, so an old password stops working once the new key data is seen. Another PC that adds the folder opens it with the password. After unlock, items still in the data folder move into their folder in the background (`Vault::relocate`). "Stop keeping library info" removes everything in `.memora` except `vault`; file events in `.memoraault` don't wake the library-info reader. Tests: `items_stay_in_their_photos_folder`, `a_folder_that_was_away_gets_the_new_password`, `items_kept_at_home_move_into_their_folder`. Checked in the app: the scratch library's 1 GB video moved from the data folder to `Photos\.memoraault` on unlock and still plays (Range 206).

**v0.4: Memories** (2026-11-18)
- `core/memories.rs` + migration 14 (`memories`, `memory_removed`): on this day (±3 days, per earlier year, 3+ items after folding bursts), trips (home = the place with the most photo days; consecutive days mostly >100 km away, one empty day and days without GPS allowed; 2+ days with 8+ items, or 1 day with 20+; "Trip to A and B"), people (named, not hidden: "over the years" with 12+ items over 2+ years, "<name> in <last year>" with 15+), best of each past year (40+), highlights of the last 3 months (25+). Picking: junk left out (screenshot/screen recording/WhatsApp Sent names, PNG/GIF/WebP without a camera, CLIP hits for screenshot/document/receipt prompts), near-duplicates folded (phash ≤ 10 within 10 min), the best item of each equal-count time slice, videos at most a quarter, cover = best photo. Stable keys keep seen/hidden/removed; hidden memories that stop qualifying stay inactive. Lists only show visible items (Archive, trash and the locked folder take items out at once).
- Server: `/api/memories` (+ `?hidden=true`), `/{id}`, PATCH `{hidden}`, `/{id}/seen`, `/{id}/remove`, `/rebuild`. Rebuild task at start, on day change, and after library changes (at least 10 min apart, while indexing is quiet); ~20 ms for 180 items. Setting `memories`. TV: `memories` in `/tv/api/home`, `items?scope=memory&id=` (404 when off).
- UI: carousel above the Photos timeline (`PhotoGrid` `top` band that scrolls with the grid), Memories page (sections, Show hidden, right-click Play/Hide), `MemoryPlayer` story viewer (progress bars, 5 s photos with a slow zoom, muted videos up to 15 s, click left/right, hold to pause, Space/←/→/M/Esc, Open this photo in the normal viewer, Remove from memory, Hide memory), Memories switch in Settings. TV page: Memories row first; a memory opens as a running slideshow.
- Tests: on this day (window, bursts, screenshots, day change), trips (two cities, day without GPS, short trip ignored), people and best of (favorite picked, time order, cover is a photo, month highlights), seen/hidden/removed/archived across rebuilds, curation spread and video cap; server `memories_row` (TV row, archived item leaves, switched off). photos-core 109, photos-ml 8, photos-server 34 tests pass; clippy and `npm run check` clean.
- Checked in the running app on the scratch library: "2 years ago", "Best of 2025", "Trip to Vagator and Calangute" (10–13 Sept 2025) and "Trip to Cherrapunji" (15–22 Apr 2025) found from real GPS; carousel, Memories page, story player (advances, ←/→, Space, Esc, marked seen). Fixed after looking: the recovery key wrapped in the middle of a group. Not tried: the TV row on a TV, people memories (no named people in the scratch library), CLIP junk filtering on real screenshots.

**v0.4: Photo editing** (2026-11-18)
- Non-destructive: an edit is a JSON recipe (`core/edit.rs` `Edit`: `rotate` quarter turns, `flip`, `straighten` degrees, `crop` in fractions of the turned picture, 15 `adjust` sliders -100..100, `filter` + `filterStrength`, background `blur`). Migration 15: `media.edit`, `edited_at` (unix ms, at least 1 s after the previous version so `?v=` URLs change, also on revert), `edit_width`/`edit_height`. `Media::version()` = max(mtime, edited_at) decides cache freshness; HEIC/RAW base copies and masks only follow the file. The timeline's `w`/`h`/`m` use the edited size and version. Recorded in `.memora` as field `edit` (value: the recipe, or null), seeded, applied by comparing normalized recipes. Videos can't be edited.
- One renderer (`edit/render.rs`), used for every picture (thumbnail, viewer copy `cache/edited`, TV, DLNA) and for the editor's live previews (POST `/api/media/{id}/edit/preview` returns a JPEG; the UI asks again on every change, one request in flight, 900 px drafts while a slider is dragged), so the preview and the result can't drift apart. Linear-light pipeline: bilinear sampling through the geometry, background blur, white balance (luminance-neutral channel gains), exposure (±2 EV), dehaze (dark channel at map size), tone on perceptual luminance with colours scaled to follow (blacks, whites, shadows/highlights against a wide blurred map, clarity against a narrow one, contrast curve around mid-grey), saturation/vibrance in Oklab (skin hues protected), a soft shoulder only when something brightens, the filter's look mixed by strength, vignette, grain (value noise in cells of 1/1400 of the long side, faded at small sizes), luminance-only unsharp mask. Neighbourhood effects use 256 px maps sampled 4x4 per cell, so a thumbnail and a full export look alike. ~13 ms for a plain 1600 px preview, 29 ms with shadows/highlights/clarity, 49 ms with a film filter (8-core PC).
- Filters (`edit/filters.rs`): 26 parametric looks in 5 groups (Everyday, Film, Cinematic, Vintage, Black & white): tone curves per channel (monotone cubic), Oklab hue bands (shift, chroma, lightness), split toning, B&W channel mixes, halation glow, grain, vignette. Film looks are our own, made from the stocks' described traits (Portra, 400H, Ektar, Velvia, Classic Chrome, Classic Negative, Nostalgic Negative, CineStill 800T, Gold, Tri-X); names avoid trademarks. Chosen over bundled 3D LUTs: the free LUT packs found had unclear or share-alike licences, and parameters work at any strength and size.
- AI: Enhance (`edit/auto.rs`) works from the picture's statistics: centre-weighted brightness with a dead zone (a bright sky alone doesn't darken a good photo), clipped highlights, deep shadows, spread, near-neutral cast (partly corrected, less for food/night), colourfulness; tuned by the scene. Scene = best match of the item's stored CLIP vector against 15 scene texts (`Ai::scene`), which also picks suggested filters. Background blur: BiRefNet-lite (MIT, ONNX fp32 224 MB, `models/birefnet-lite/model.onnx`, optional) finds the subject at 1024x1024 (`ml/segment.rs`); the mask is cached as `cache/mask/<id>.png`, made when blur is first used or when a synced edit with blur arrives; the blur is weighted by background so the subject doesn't bleed. First use ~20 s (model load on the GPU), then instant.
- Background blur, reworked after use (2026-11-18): the Gaussian looked smeared, not optical. `edit/bokeh.rs` styles (`Edit.blur_style`): Lens (flat disc kernel with a soft edge, clipped highlights boosted up to 2.6x so they bloom into bokeh balls, and a second smaller disc mixed in near the subject as a stand-in for depth), Oval (anamorphic: disc 0.62 wide), Soft (the old Gaussian), Swirl (tangential blur growing towards the corners, Petzval/Helios style), Motion (two box blurs along rows), Zoom (streaks towards the mask's centroid). All at a work size where the radius (up to 3 % of the long side) is 6-8 px, sampled 1-3x per pixel each way from the source instead of shrinking it first. ~45-60 ms for a 1600 px preview, ~25-40 ms for 900 px drafts. The editor keeps 2560, 1280 and 720 px copies of the photo and renders from the smallest that is big enough.
- Without a graphics card: a second export of the subject model at 512x512 (studioludens, MIT, 192 MB, `models/birefnet-lite-512/model.onnx`) is used on the CPU, with all but one core (`foreground_threads`; recognition in the background keeps its quarter). Measured on this PC's CPU: 1024 model 3.7 s to load + 6.4 s per photo, 512 model 1.8 s + 1.2 s. The 1024 model is used whenever DirectML works. `/api/edit/filters` says `subjectsGpu` (known after first use, else guessed from the setting and where recognition runs); the editor then says a dedicated graphics card is required for better performance. The blur starts with a "Blur the background" button (finds the subject, then shows the style chips and slider at 50), so nobody waits on a slider.
- Server (`server/src/edit.rs`): `/api/edit/filters` (+ whether blur is available), GET/PUT `/api/media/{id}/edit`, `/preview`, `/enhance`, `/subject` (503 without the model), `/export` (full-size JPEG with a small EXIF block: date taken, camera, upright; never over the original). Decoded photos for the editor are kept for the last 3 (2560 px and a 720 px copy for filter tiles).
- UI: `Editor.svelte` over the viewer (Edit button, E, right-click → Edit in the viewer and the grid): tabs Suggestions (Enhance, suggested filters, background blur), Crop (rotate, flip, straighten that shrinks the frame to stay inside the turned photo, aspect presets, drag frame and handles), Adjust (Light, Colour, Detail and effects; double-click resets), Filters (live tiles of every filter on the photo, strength slider); undo/redo, hold to compare with the original (`\`), Save, Revert, Save copy. Info panel shows "Edited".
- Tests: renderer (identity, turns/flip/crop pixel positions, straighten, sizes alike at every resolution, sliders, every filter, strength 0, background blur), Enhance (dark photo with a cast, well-exposed photo with a bright sky), recipes cleaned, save/revert/sizes, edited pictures, export with EXIF, `.memora` sync of edits, subject model on a test picture (skipped without the model), server `photo_editing`. photos-core 131, photos-ml 9, photos-server 35 pass; clippy and `npm run check` clean.
- Checked in the running app (scratch library of 30 photos copied from `I:\My Photos`): Enhance on a portrait (first version darkened it by 1 EV because of the sky; fixed with the centre-weighted dead zone), filter tiles and picking, square crop with 5° straighten (render matches the crop screen), background blur (clean mask around cap and glasses), Save (grid and viewer show the edit), Revert. Fixed after looking: the editor didn't open on an already edited photo (a Svelte state proxy can't be cloned). Not tried: Save copy's file dialog (export covered by tests), the TV page with an edited photo, background blur on the CPU only.

Changes from the original plan:
- Faces are grouped by average linkage, not DBSCAN. CLIP search uses a neutral-text baseline and a relative cut-off. CLIP sees the picture's centre square (panorama edges are cut).
- The TV web page has no People row yet (DLNA has a People folder).
- No `jobs` table: pending work is the `media.stage` column (0 new → 1 metadata → 2 thumbnail). Later phases add stages.
- Thumbnails are JPEG (400 px tall, max 1600 wide), cached by media id at `cache/thumb/<id/1000>/<id>.jpg`, so renames keep them. JPEG instead of WebP because the `image` crate only writes lossless WebP, and DLNA needs JPEG thumbnails anyway.
- `blake3` hashing moved to Phase 4 (duplicates).
- Album items are sorted by date like every other grid; there is no manual item order (the day-grouped grid needs date order). Albums themselves are listed newest first.
- FTS covers file/folder names, camera, album and place names; people names and picture content are matched separately (Phase 6).
- TV page is plain TypeScript, not Svelte (old TV browsers). DLNA has no `Search` action (TVs browse).
- Only an NSIS installer (no MSI): the firewall rule is an NSIS hook. FFmpeg and the models are plain resources next to memora.exe, not Tauri sidecars.
- No street-level map: the offline basemap stops at cities. An optional `.pmtiles` detailed basemap is not built. No "not a duplicate" memory for false positives yet.

### Remaining

**v0.3 (planned 2026-11-11)**: ~~custom title bar~~, ~~right-click menus~~, ~~Archive~~, ~~movable data folder~~, ~~`.memora` library info sync~~, ~~`.memora` index snapshot~~, ~~backup file~~ (all done). Design, steps and estimate in "Planned: Custom title bar, …" after the Progress section.

**Packaging (v0.2 leftovers)**
- v0.2 released unsigned (decided 2026-11-18): version 0.2.0, installer built without a certificate; "Signed installer" moved to the v0.4+ list in the roadmap.
- A code-signing certificate (options in `scripts/sign.ps1`; Azure Trusted Signing is limited to some countries for individuals; Certum's open-source certificate or SignPath's free OSS program are alternatives). Then set `MEMORA_REQUIRE_SIGNING=1` for releases.
- Test the installer on a clean PC or VM: install (UAC), firewall rule present (`netsh advfirewall firewall show rule name="Memora (TV sharing)"`), FFmpeg/models found next to the exe, TV sharing works on a Private network, uninstall removes the rule.
- ~~Run `heif_layout_matches_ffmpeg` with the bundled FFmpeg 7.1.1 first on `PATH`~~ Done 2026-11-18: same layout for all 2,264 HEIC photos in the library.
- ~~Bump the version to 0.2.0~~ Done 2026-11-18 (workspace `Cargo.toml`, the path dependencies in `crates/ml` and `crates/server`, `tauri.conf.json`).

**Phase 6 follow-ups (optional, for PCs without a strong GPU)**
- Pause recognition while a laptop runs on battery.
- Run recognition only while the PC is idle (its threads already run at below-normal priority).
- A "faces only" option that skips CLIP (saves about a third of the time).
- People row on the TV web page.

### Open issues / to verify
- RAW: packed 10/12/14-bit or compressed sensor data without a preview is not developed (shown as an error tile); every camera format tested so far has a preview. Only DNGs were tested on real files (the other formats with synthetic files).
- HDR: 4K60 HDR converts at about real time on this PC; a slower CPU will stutter on those (4K30 and 1080p are fine). Tone mapping needs FFmpeg with zimg (the bundled build has it).
- Video dates: the local-time check needs the file's modification time to be from the recording; files copied in a way that resets it are read as UTC (as before).
- Videos in codecs the webview can't play now play as a converted copy (skipping ahead is limited there); "Open in default player" stays as a fallback.
- **Test on the Samsung TV**: the source list entry, browsing, photos, an HEVC phone video (original), an MKV/odd file (converted), seeking, and the `/tv` page in the TV browser (which may use a pointer instead of arrow keys). A device at 192.168.0.171 found and browsed the server during testing (probably the TV).
- DLNA can't have a PIN; anyone on the Wi-Fi can browse while it's on (said in Settings).
- Firewall: the installer's rule covers private and domain networks only; on a network marked Public, TVs can't connect (said in Settings). In dev builds Windows asks the first time.
- Phase 6: a person can come out split in two (Merge fixes it); content search favours precision, so broad words ("flowers") may find only a few photos; a search word that is also in the folder path (e.g. the Windows user name under `C:\Users\<name>`) matches every file there, because the file-name index covers the whole path; no People row on the TV page yet.
- Phase 6 timings not measured yet: a full library run in processor-only mode (the CPU numbers above are estimates from per-model timings; switching to the CPU was tested), and DirectML speed on integrated graphics.
- Not exercised in the running app yet: the viewer's actions (favorite, trash, add to album, use as cover), the album picker and album pages, Empty trash / Delete forever (needs a real Recycle Bin run), the search filters (type, camera, dates), light theme, and the Places detail page and map in a production build (`cargo tauri build`). The v0.3 actions refactor (`lib/actions.ts`) touches all of the viewer and selection actions, so they get clicked through then.
- Svelte logs a dev-only warning for `library.status ??= s` in `lib/library.svelte.ts` (`assignment_value_stale`); harmless, worth tidying.
- Emptying the trash on a removable drive without a Recycle Bin is untested.
- The first run after the Phase 4 upgrade reads every file once (blake3), so indexing shows "Indexing… N left" for a while on a big library.
- The `I:` drive is nearly full (3.9 GB free on 2026-11-05). Build output now goes to `C:/Users/Akash/AppData/Local/LocalPhotosBuild/target` (set in `.cargo/config.toml`); the old `target/` folder in this project (12+ GB) is unused and can be deleted. v0.3's "move data folder" lets the library data (16 MB database + 1.8 GB cache) live on another drive too.

---

## Planned: Custom title bar, right-click menu, archive, portable library  (added 2026-11-11; all done: D, A, B, C1, C2a, C2b 2026-11-11, C3 2026-11-18)

Order: (D) custom title bar first, because it changes the app shell and the viewer's top bar that everything else sits in; then (A) right-click menu, because it also gives Archive its menu entries; then (B) Archive; then (C) portability, in steps that each work on their own.

Decided with the user (2026-11-11): custom Svelte context menu (not the native one); trash is synced across PCs; the `.memora` index snapshot is stored by default; a self-designed title bar replaces the Windows one.

### A. Right-click menu for photos and videos

**Approach** (decided): our own Svelte menu (`components/ContextMenu.svelte`), not Tauri's native popup menu. It follows the app theme, can show icons and a "3 items" header, works in the dev browser, and the same code can later open on long-press. The WebView2 default menu ("Back", "Inspect", "Save image as") is switched off everywhere except text fields, where Cut/Copy/Paste stay.

**Shared actions first**: favorite, archive, add to album, trash/restore/delete, cover, Show in Explorer and Open with default app now live separately in `Viewer.svelte` and `MediaPage.svelte`. Move them into `lib/actions.ts` (each action: label, icon, shortcut, `enabled(items, context)`, `run(items)` with its Undo toast). Selection bar, viewer toolbar and context menu all use the same list, so they cannot drift apart.

**What it shows**

| Right-click on | Items |
|---|---|
| Photo tile | Open · Add to / Remove from favorites · Add to album… · Archive / Unarchive · Use as album cover (album page) · Remove from this album (album page) · — · Copy image · Copy file path · Show in Explorer · Open with default app · Set as desktop background · — · Info · Select · — · Move to trash |
| Video tile | Play · same as photo, minus Copy image and Set as desktop background; "Open with default player" |
| Tile that is part of a selection | Header "N items selected" · Favorites · Add to album… · Archive · Copy file paths · Show in Explorer (only for 1 item) · Move to trash |
| Tile outside the current selection | Like Windows Explorer: the selection is replaced by that item, then the single-item menu |
| Trash page | Restore · Delete forever · Show in Explorer |
| Archive page | Unarchive instead of Archive |
| Photo/video in the viewer | Same as the tile menu, without Open/Select |
| Missing file / broken tile | Show folder in Explorer · Remove from library (only if the folder is reachable) |
| Later: album, person, place cards | Open · Rename · Delete album / Hide person / Merge… |

**Details**
- Opens at the pointer, flips to stay inside the window; also opens with Shift+F10 or the Menu key on the focused tile. Arrow keys, Enter, Esc; `role="menu"`.
- Closes on scroll, resize, route change and on a click elsewhere.
- New Tauri command `set_wallpaper(id)`; Copy image ended up in the page (canvas → PNG → `navigator.clipboard.write`), so no clipboard plugin. Planned was: `copy_image(id)` (puts the display JPEG on the clipboard, so HEIC/RAW work; `tauri-plugin-clipboard-manager`) and `set_wallpaper(id)` (writes an upright JPEG to `%LOCALAPPDATA%\Memora\wallpaper.jpg`, then `IDesktopWallpaper`). Copy file path uses `navigator.clipboard`.
- Files are never edited: no Rotate or Edit entries (same rule as today).

### B. Archive (like Google Photos)

Archived items are not deleted, but they show up **only in the Archive section** (desktop Archive page, TV page Archive row, DLNA Archive folder). Everywhere else they are treated as if they were not there.

| Place | Archived items |
|---|---|
| Archive page (new, in the sidebar under Favorites) | shown |
| Photos timeline, scrubber, Favorites, Albums, Search, People, Places, Map, Videos, Duplicates | hidden (album/person/place counts and covers skip them; a cover that gets archived falls back to the next item) |
| TV page | new **Archive** row on the home screen + Archive grid; hidden from every other row |
| DLNA | new top-level **Archive** folder (flat, newest first, paged like the others); hidden from Timeline, Albums, People, Places, Favorites, Videos, Folders |
| Trash | trash wins (shown only in Trash); restoring an item puts it back in the Archive |
| Album membership, favorite flag, face names | kept, so Unarchive brings the item back exactly where it was |
| Recognition, hashing | still run, so Unarchive needs no extra work |

- Migration 9: `media.archived_at INTEGER` + index. `Scope::Archive`; every other scope and every other query that lists items (`browse`, `places`, `people`, `duplicates`, `map/points`, `cameras`, semantic search, DLNA, TV) adds `archived_at IS NULL`. To make this hard to miss, the listing queries go through one shared `visible` SQL condition (`missing = 0 AND trashed_at IS NULL AND archived_at IS NULL`). Library revision is bumped so open views reload.
- Core: `organize::archive(ids)` / `unarchive(ids)`. API: `POST /api/media/archive|unarchive`, `/api/timeline?scope=archive`, `archived` in media details. Opening an archived item by id still works (viewer from the Archive page).
- TV and DLNA: Archive row/folder can be switched off in Settings (`lanArchive`, on by default). DLNA has no PIN, so anyone on the Wi-Fi can open the Archive folder while it's on; Settings says so. On the TV page, the PIN (if set) covers it like everything else.
- UI: Archive action in selection bar, viewer and right-click menu, shortcut Shift+A (as in Google Photos), toast with Undo, "Archived" line in the info panel, Archive page (`MediaPage` with `scope=archive`, empty-state text explaining what archive does).
- Tests: archived item absent from timeline, favorites, album, search (text, person, CLIP), places, map, duplicates, people counts and covers; present in Archive; trash + restore returns it to Archive; DLNA Archive folder lists it and no other DLNA folder does; TV Archive row lists it and `Recent` doesn't; `lanArchive` off removes both.

### C. Portable library (changes on PC 1 show up on PC 2)

**Why not just move the database to a shared or cloud folder**
- SQLite in WAL mode on OneDrive/Dropbox/Google Drive or a network share can be corrupted: the sync client copies `library.db` while `-wal` is half written, and file locking does not work across PCs.
- Two PCs with the same database open at once would overwrite each other.
- The database stores absolute paths (`I:\My Photos\...`); on PC 2 the same folder may be `D:\Photos`.
- Most of the data folder is machine-specific and can be rebuilt: the index, 1.8 GB of thumbnails and caches, face vectors and CLIP vectors.

**What is worth carrying over** is the small part you made yourself: favorites, archive, trash, albums (name, items, cover), people (names, hidden, merges, "Not this person", manual moves) and a few preferences (theme). Everything else is rebuilt on each PC from the photos.

So yes, there should be an option to change the storage location, but for **disk space** (the `I:` drive is nearly full), not for syncing. Syncing uses a `.memora` folder inside each watched folder (C2).

**C1. Move the data folder, relink folders** (useful by itself, and needed for a new PC)
- Settings → Storage: "Library data folder" with Move… (copy `library.db` + `cache`, check, switch, then delete the old copy). The location is kept in `%LOCALAPPDATA%\Memora\location.json`, because the database can't store its own location. Refuses cloud-synced folders (OneDrive/Dropbox/Google Drive paths) and network drives, with a short reason.
- "Locate folder…" for a watched folder that is not available (drive letter changed, new PC): rewrites the path prefix of its `media` rows, so nothing is re-indexed and favorites/albums stay.

**C2. A `.memora` folder in each watched folder** (the library info travels with the photos)

When a folder is added, Memora looks for `<folder>\.memora\`. If it is there, the folder's favorites, archive, trash, albums, people names and corrections are read from it (and, if present, the index snapshot, so the heavy work is skipped). From then on, this PC's changes for items in that folder are written there too. Plug the external drive into PC 2, add the folder, and everything is as it was on PC 1.

*Layout (one per watched folder, not per subfolder):*
```
<watched folder>\.memora\            hidden folder
  memora.json                        format version, folder id (uuid), created by / when
  changes\<device-id>.jsonl          this PC's edits, append-only; each PC writes only its own file
  snapshot\<device-id>.json          periodic compacted state of the edits (so logs stay short)
  index\<device-id>.db               optional: SQLite snapshot of the index (see below)
```

*Why a folder of per-PC files and not one shared SQLite database in `.memora`:*
- One SQLite file written by two PCs breaks in exactly the cases this feature is for: OneDrive/Dropbox/Syncthing copy it while it's half written or make "conflicted copy" files, and on a NAS (SMB share) SQLite's file locking is unreliable. It only works safely on a USB drive used by one PC at a time.
- With one file per PC, nothing is ever written by two PCs. Each PC only appends to its own log and reads the others', so it works the same on a USB drive, a NAS, and a cloud-synced folder, and a half-synced log just ends in an incomplete last line, which is skipped until the rest arrives.
- SQLite is still used where it is safe: `index\<device-id>.db` is written to a temp file and renamed over the old one, so other PCs only ever see a complete file and open it read-only.

*Your edits (`changes\*.jsonl`)*
- One line per change: `{clock, device, op, item:{p, h}, value}`; `p` = path relative to the watched folder, `h` = `blake3` hash (so a moved or renamed file is still recognised). Ops: favorite, archive, trash/restore, album create/rename/delete/cover, album add/remove, person name/hide/merge, face move / "Not this person".
- Written to an `outbox` table in the same transaction as the change itself, then flushed to the file, so a crash or an unplugged drive loses nothing; the outbox is flushed when the drive comes back.
- Merge: last change wins per item and field, by a hybrid logical clock (time + counter + device id), so clocks that are a bit off don't matter.
- Albums and people span folders: they get stable ids (`albums.uid`, `persons.uid`). Each `.memora` stores the album/person name and cover with its clock, plus membership for its own items. Adding a second folder that knows the same album merges into it by id.
- People on PC 2: recognition runs (or comes from the index snapshot), then names, merges and corrections are applied to faces with the same item and box (existing IoU > 0.5 rule).
- Each PC watches `.memora\changes` and `snapshot` (the watcher already sees the folder; dot-folders stay out of the scan) and applies new lines when they arrive.

*Index snapshot (`index\<device-id>.db`, on by default, can be switched off per folder)*
- Per item: relative path, size, mtime, hash, metadata, perceptual hash, place, face boxes + vectors, CLIP vector. Written after indexing settles (at most every few minutes).
- When PC 2 adds the folder, items whose size + mtime (or hash) match are copied from the newest snapshot instead of being processed: no metadata probing, hashing or recognition. Only thumbnails are made again (thumbnails stay in the local cache: the cache is 1.8 GB for this library, too much to push through cloud sync, and quick to rebuild). Snapshots from another app or model version are ignored for the parts that changed.
- Size: about 3 KB per item (mostly the CLIP vector), so ~10 MB for 3,000 items, ~300 MB for 100,000.

*Rules and edge cases*
- Asked once when adding a folder: "Keep library info in this folder?" (default yes). If the folder is read-only (DVD, no permission, read-only share), the info stays on this PC and Settings says so; it can be turned on later.
- Adding a subfolder of a folder that has a `.memora` higher up (e.g. PC 1 watches `I:\My Photos`, PC 2 only `I:\My Photos\2024`): Memora uses the parent's `.memora` and only items under the subfolder.
- Removing a watched folder never deletes its `.memora`. Settings has "Remove library info from this folder" for that.
- The local `library.db` stays the working copy (fast queries, works when the drive is unplugged); `.memora` is the shared record it syncs with.
- Not stored in `.memora`: watched folder list, LAN/TV settings, GPU choice, thumbnails/caches, the vault. Preferences like theme stay per PC.
- Note: this writes one hidden folder into your photo folders, a change from "never touch photo folders" (photos themselves are still never changed or moved). Phone backup tools that upload a whole folder may upload `.memora` too.

**C3. Backup file**
- "Export library info" writes one `.memora-backup` zip of all folders' `.memora` contents (without index snapshots) plus local-only info; "Import" merges it. For backups and for folders kept on read-only drives.

**Caveats**
- Photos stored "online only" in OneDrive (Files On-Demand) get downloaded when Memora makes thumbnails. The Settings text should say so; skipping online-only files can be an option.
- Trash is synced. If each PC has its own copy of the photos, emptying the trash (or the 30-day purge) on PC 2 also sends PC 2's copy to its Recycle Bin. That is intended (confirmed: trash on one PC = trash everywhere); the "Empty trash" dialog says it also applies to the other PCs.
- Edits for an item PC 2 hasn't hashed yet are matched by relative path first, so they apply right away when the path is the same; renamed files wait for the hash.

### D. Custom title bar (replaces the Windows title bar)

The Windows title bar ("Memora" + – ☐ ✕ on a white/grey strip) goes away. The app's own top bar (logo, search, indexing chip, settings) becomes the title bar and gets its own window buttons, like Google Chrome, Edge or the Windows 11 Photos app.

*Layout (one 48 px row, colours from the app theme in light and dark):*
```
[logo Memora] [   Search people, places, things…   ] [Indexing… 120 left] [⚙]  ··drag··  [ – ][ ☐ ][ ✕ ]
```
- Window buttons: 46 px wide, full bar height, Windows 11 glyphs (Segoe Fluent Icons, falling back to Segoe MDL2 Assets on Windows 10): minimize, maximize ↔ restore (icon swaps when maximized), close. Hover grey; close hover red (`#C42B1C`, white glyph); pressed state; tooltips.
- Everything that is not a control is a drag area (`data-tauri-drag-region`): drag to move, drag to a screen edge to snap, double-click to maximize/restore.
- Right-click on an empty part of the bar (or Alt+Space) opens the normal Windows window menu (Restore, Move, Size, Minimize, Maximize, Close) through a small `show_system_menu` command (`GetSystemMenu` + `TrackPopupMenu`). Our own context menu (A) never opens there.
- Inactive window: bar text and buttons are dimmed, like native windows.
- The photo viewer (full-window, black) keeps a slim version: its own toolbar row gets the same window buttons and drag area, so the window can be moved, minimized and closed while a photo is open. Dialogs and their dark backdrop stay below the window buttons, so Close always works.
- Close does the same as before (DLNA goodbye, FFmpeg stopped, app exits); Alt+F4, Win+↑/↓/←/→ keep working because they are handled by Windows.

*Implementation*
- `tauri.conf.json`: `"decorations": false`, `"shadow": true` (keeps the Windows 11 shadow and rounded corners); Tauri 2 keeps resize borders on undecorated windows on Windows.
- Capabilities: `core:window:allow-minimize`, `allow-toggle-maximize`, `allow-close`, `allow-start-dragging`, `allow-internal-toggle-maximize`, `allow-is-maximized`.
- `components/WindowControls.svelte` (used in the top bar and the viewer) + `lib/window.ts` (maximized/focused state from `onResized` / `onFocusChanged`).
- Snap Layouts (the layout picker that appears when hovering the maximize button on Windows 11) are lost with a plain custom button. Plan: subclass the window (`SetWindowSubclass`, `windows` crate) and answer `WM_NCHITTEST` with `HTMAXBUTTON` over the maximize button's area (the UI sends its position on resize). Needs a short spike, because WebView2 sits on top of the window and receives the mouse first; if it can't be made reliable, the button keeps working without the picker and Win+Z still opens it.
- Maximized windows: check nothing is cut off at the screen edges (undecorated windows can overhang by the border width), including on a second monitor with different scaling.
- Remember window size, position and maximized state between launches (`tauri-plugin-window-state`).
- In the dev browser (no Tauri), the window buttons are hidden.

*Checks (by hand, in `cargo tauri dev` and the installed build):* move, snap to edges and corners, double-click maximize, resize from every edge and corner, Win+arrows, Alt+Space menu, minimize/restore from the taskbar, maximized on both monitors at different DPI, light/dark and inactive colours, shadow and rounded corners on Windows 11, viewer open, a dialog open, Snap Layouts on hover.

### Steps
1. D: custom title bar (`decorations: false`, `WindowControls.svelte` in the top bar and viewer, drag areas, system menu command, window state plugin), then the Snap Layouts spike.
2. A: `lib/actions.ts` refactor, `ContextMenu.svelte`, menus for grid, viewer, trash; `copy_image` / `set_wallpaper` commands.
3. B: Archive: migration 9, shared `visible` condition in every listing query, core + API, DLNA Archive folder, TV Archive row, `lanArchive` setting, UI (page, actions, Shift+A), tests.
4. C1: data folder move + location file, "Locate folder…", tests (move round trip, path prefix rewrite).
5. C2a: migration 10 (uids, outbox, per-folder cursors), `.memora` layout, change log writer/reader, merge, watcher, "keep info in this folder" prompt, read-only fallback, tests (two libraries in temp folders sharing one photo folder with different drive letters, out-of-order and concurrent edits, clock skew, half-written last line, unplugged drive, subfolder of a `.memora` folder, trash emptied on one PC).
6. C2b: index snapshot writer (temp + rename, on by default) and reuse on add, version checks, tests (PC 2 adds the folder and skips probing/recognition; changed files are processed).
7. C3: backup export/import.

### Effort estimate (same basis as above)
| Step | Input tokens (mostly cached) | Output tokens |
|---|---|---|
| 1. Custom title bar (+ Snap Layouts spike) | 1-3M | 8-15k |
| 2. Shared actions + right-click menu | 1-3M | 10-20k |
| 3. Archive (everywhere hidden, TV/DLNA Archive) | 2-3M | 12-20k |
| 4. Move data folder, locate folder | 1-2M | 8-12k |
| 5. `.memora` change logs + merge | 4-7M | 30-50k |
| 6. Index snapshot | 2-3M | 12-20k |
| 7. Backup file | 1-2M | 6-10k |
| **Total** | **~12-23M** | **~86-147k** (about 5-6 days) |

### Open decisions
- ~~Sync: should archive/favorites apply to every exact copy of a file or only the one at the same relative path~~ Built as suggested: the item at the same relative path, else every copy with the same hash.
- Optional, separate from the above: an HDR-keeping conversion (HEVC 10-bit with HDR10/HLG, NVENC) for HDR TVs that can't play the original container; today converted streams are always SDR.

---

## Planned: Private vault ("Locked folder")  (added 2026-11-05, not started)

Goal: private photos and videos are **encrypted on disk** and open only inside this app after a password. Hiding by attribute or folder name is not enough; files must be unreadable from Explorer or other apps. The vault is never exposed on the LAN, DLNA or `/tv`.

### Design
**Storage**
- Vault folder (configurable, default `<data folder>\vault\ab\<random-id>.enc`): random names, no extension, originals removed.
- Metadata (original name, dates, EXIF, GPS, dimensions) in a separate database `vault.db`, never in `library.db` or `media_fts`. **Changed 2026-11-18 (decided with the user): plain SQLite with every row encrypted, not SQLCipher.** SQLCipher needs OpenSSL (Perl/NASM to build it on Windows); per-row XChaCha20-Poly1305 under the master key uses the same crypto as the files. The user's requirement: nothing readable on disk. So `vault.db` has no plaintext columns at all: `meta(key, value)` holds only the key-wrapping data (salt, Argon2 parameters, the master key wrapped twice), `items(id, data)` a random id and the sealed record (name, path, dates, GPS, camera, sizes, the item's file key and its file names). Sorting and listing happen in memory after unlock. `PRAGMA secure_delete = ON`. What someone browsing the disk can see: that a vault exists, roughly how many items and how many bytes (the same as with SQLCipher).
- Thumbnails and the JPEG preview of HEIC/RAW are encrypted too. They are taken from the library's cache at import time (the item is still in the library then; those plaintext cache files are deleted right after), so later viewing never needs plaintext. Every encrypted file gets its own random name (`files\ab\<random>`, no extension), so the files of one item can't be told apart from another's.
- **Vault responses are sent with `Cache-Control: no-store`**, so WebView2 never writes a decrypted picture to its disk cache (`EBWebView\...\Cache`). Found while designing; normal thumbnails are cached for a year.

**Crypto** (audited RustCrypto crates: `argon2`, `chacha20poly1305`, `zeroize`; `blake3`, already used, derives the recovery key's wrapping key)
- Password -> **Argon2id** -> key-encryption key that wraps a random 256-bit **master key**. Changing the password only re-wraps the master key.
- **Recovery key** (random 160 bits, shown once as 8 groups of 4 letters/digits, Crockford base32) also wraps the master key (wrapping key = `blake3::derive_key`; it has full entropy, so no Argon2 needed). Forgotten password without it = data lost.
- Argon2id: 256 MiB, 3 passes, 4 lanes (~0.1 s on this PC: the lanes run in parallel; 64 MiB took only 22 ms, too cheap); the parameters are stored, so they can be raised later.
- Each file has its own random file key, wrapped by the master key.
- Files are encrypted in 64 KB chunks with **XChaCha20-Poly1305**; chunk index and a "final chunk" flag are in the authenticated data (stops truncation and reordering). Chunking keeps HTTP Range working, so video seeking still works.
- Versioned file header so the format can change later.

**Access**
- Unlock derives the key and keeps it in memory only (`zeroize`). Auto-lock on idle, minimize or Windows lock; the key is wiped.
- Vault endpoints exist only on the localhost API (which already needs the per-launch token) and answer 423 while locked. They are not routed on the LAN/DLNA listener. (No second vault token: the same page would hold both, so it adds nothing.)
- Auto-lock after 5 minutes without vault activity (setting: 1/5/15 min), optionally when Memora is minimized, when Windows locks (`WTSRegisterSessionNotification` in the existing window subclass), and with the Lock button.
- Vault items are excluded from timeline, search, map, places, duplicates, faces, CLIP, trash, albums, favorites, Archive, DLNA and `/tv`, and from everything v0.3 writes outside the PC's data folder: `.memora` change logs and index snapshots, and the library info backup file.
- "Open in default player" and "Show in Explorer" are disabled for vault items.
- Later, optional: Windows Hello unlock; exclude the window from screen capture (`WDA_EXCLUDEFROMCAPTURE`).

**"Move to locked folder" flow**
1. Select items -> action -> password (unlock if locked).
2. Per item: extract metadata, make thumbnails/preview, encrypt, then **decrypt and compare the hash**.
3. Only after verification delete the original (permanent delete, **not** the Recycle Bin, which would keep a plaintext copy).
4. Purge everything derived from it: `media` row, `cache/thumb`, `cache/tv`, `cache/tn`, FTS entry, album/favorite/archive links, place, face and CLIP data, and its entries in the folder's `.memora` (this PC's change log is compacted without it and the index snapshot rewritten). Remove the row before the file so the watcher does not treat it as "missing".
5. "Remove from vault" decrypts back to a folder the user picks.

### Caveats
- This **breaks the "index in place, never move files" rule** for vault items; the UI must say so.
- Deleting the original on an SSD does not guarantee the data is wiped (TRIM/remnants). Recommended companion: **BitLocker / BitLocker To Go** on the drives (free on Windows 11 Pro). Layers: BitLocker protects a stolen or lost drive; the vault protects against anyone using the logged-in PC or other apps. A VeraCrypt container is an alternative without code, but integrates less cleanly.
- Malware on a PC while the vault is unlocked can read what is being viewed; auto-lock reduces this but cannot remove it.
- The `I:` drive is nearly full, so the vault location must be configurable (by default it moves with the data folder, v0.3). It must not be inside a watched folder.
- `.memora` (v0.3): other PCs' change logs and index snapshots may still hold the moved item's file name and hash until those PCs run and compact them; this PC can only clean its own files. The UI should say so for folders that sync with other PCs.
- The vault is not synced between PCs in the first version; it lives on one PC (moving it means exporting and importing).
- Encrypted videos cannot be played by external players. Videos the webview can't decode could be converted by FFmpeg reading the decrypted stream from a localhost URL (HTTP input, so seeking works); first version: originals only, with the "can't play" message.
- Hibernation file and page file may hold key material; BitLocker covers this.

### Steps
1. Crypto module (Argon2id, key wrapping, chunked AEAD) with tests: round trip, tamper/truncate/reorder detection, wrong password, seek to any offset.
2. `vault.db` (SQLCipher), setup/unlock/lock/change password/recovery, import and export with verify-then-delete and cache purge.
3. Localhost streaming: decrypt on the fly with Range, encrypted thumbnails/previews, unlocked-session check.
4. UI: Locked folder page, setup (password + recovery key), unlock screen, auto-lock, "Move to locked folder" in the selection bar and viewer.
5. Exclusion from every other feature (timeline, search, map, duplicates, ML, Archive, TV, DLNA, `.memora`, backup file) plus integration tests (e.g. vault item never returned by `/api/timeline`, LAN listener returns 404 for it, its name is not in this PC's `.memora` files after moving it).

### Effort estimate (same basis as above)
| Step | Input tokens (mostly cached) | Output tokens |
|---|---|---|
| 1. Crypto module + tests | 1-2M | 10-20k |
| 2. Vault DB, import/export, purge | 2-4M | 20-35k |
| 3. Localhost streaming | 1-3M | 10-20k |
| 4. UI | 2-3M | 15-25k |
| 5. Exclusions + integration tests | 1-2M | 5-10k |
| **Total** | **~7-14M** | **~60-110k** (about 2-3 days) |

Windows Hello unlock would add roughly 1-2M input and 8-15k output. Biggest risks: building SQLCipher on Windows, and finding every place derived data can leak (caches, FTS, embeddings).

### Decisions
- Vault location: `<data folder>\vault`, moves with the data folder (the v0.3 move copies it, `finish_move` deletes the old one).
- First version: password + recovery key only; Windows Hello later (roadmap item).
- Per-row encryption instead of SQLCipher (2026-11-18, see Storage).

---

## Planned: Memories  (added 2026-11-18; decided with the user: all four kinds, Photos carousel, Memories page and TV row)

Goal: like Google Photos' memories. Short stories made from the library on their own, shown as cards above the timeline and in a full-screen story viewer. Everything is computed offline from what the index already has (dates, places, faces, CLIP vectors, perceptual hashes); no new recognition work.

### Kinds
| Kind | Rule | Title / subtitle |
|---|---|---|
| On this day | For each earlier year: visible items taken within today ±3 days, at least 3 after bursts are folded. Shown while today is within 3 days of the date. | "3 years ago" / "18 Nov 2023" |
| Trip | Home = the city with the most photo days. A trip = consecutive days (one empty day allowed) whose geotagged items are mostly over 100 km from home; 2+ days with 8+ items, or 1 day with 20+. Days without GPS inside the trip count. | "Trip to Gangtok" (most common city; two: "Gangtok and Darjeeling") / "2–6 Oct 2023" |
| Person | Named, not hidden people with 12+ items over at least 2 calendar years: "over the years". Plus "<name> in <year>" for the last full year with 15+ items of them. | "Asha over the years" / "2019–2026" |
| Best of | "Best of <year>" for each past year with 40+ items; "<Month> highlights" for the previous month with 25+ items. | "Best of 2025" / "50 photos" |

### Picking the photos (`core/memories.rs`, `curate`)
- Candidates: `VISIBLE` items (never archived, trashed, missing, or in the vault) at `stage >= HASH`.
- Junk left out: screenshots and documents, by file name/folder (`screenshot`, `screen recording`, WhatsApp "Sent"), PNGs without a camera, and (when recognition has run) items whose CLIP vector is closer to "a screenshot" / "a document, receipt or text" than to a few photo prompts. The prompt vectors are computed once by the ML crate and stored in settings (`_memoryPrompts`).
- Score: favorite +3, named people in it +1 each (max 3), photo over video +0.5, resolution ≥ 2 MP +0.5.
- Spread over time: the candidates are cut into as many time slices as photos wanted and the best item of each slice is taken. Near-duplicates (perceptual hash Hamming ≤ 10 within 10 minutes) keep only the best one.
- Sizes: on this day ≤ 20, trip ≤ 40, person ≤ 30, best of ≤ 50. Videos at most a quarter. Cover = best-scoring photo (not a video).

### Storage and refresh
- Migration 13: `memories(id, key UNIQUE, kind, title, subtitle, start_day, end_day, cover_id, items JSON, score, created_at, seen_at, hidden)` and `memory_removed(key, media_id)`. `key` is stable (`day:2023-11-18`, `trip:2023-10-02`, `person:<uid>`, `person:<uid>:2025`, `year:2025`, `month:2026-10`), so Hide, seen and removed photos survive a rebuild.
- Built at startup, when the day changes, and after indexing/recognition settle (at most every 10 minutes): one pass over the visible items. A memory whose items changed is updated in place (same key); memories that no longer qualify are dropped unless hidden (hidden ones are kept so they stay hidden).
- Settings: "Show memories" (on).

### UI
- **Photos page carousel** (above the first date group, scrolls with the grid): up to 12 cards, on-this-day first, then the newest unseen others. Card = cover with a gradient, title, subtitle. Hidden when there are none or the setting is off.
- **Memories page** (sidebar): sections On this day, Trips, People, Best of; card right-click: Play, Hide this memory.
- **Story viewer** (`MemoryPlayer.svelte`): full window, black; progress bars at the top, each photo 5 s with a slow zoom, videos play muted up to 15 s; click the right/left third = next/previous, Space pauses, ←/→, Esc closes; toolbar: title, Pause, Sound, "Open this photo", "Remove from memory", "Hide memory", window buttons. Marks the memory seen.
- **TV page**: a Memories row first on the home screen (when there are any); opening one plays the slideshow from the first item.
- DLNA: not planned (not asked for).

### API
- `GET /api/memories` (cards), `GET /api/memories/{id}` (+ items as timeline items), `POST /api/memories/{id}/seen`, `PATCH /api/memories/{id}` `{hidden}`, `POST /api/memories/{id}/remove` `{ids}`.
- LAN: Memories row in the TV home + `items?memory=<id>` (same visibility rules; archived items never).

### Steps
1. Migration 13, `memories.rs` (kinds, curate, rebuild with stable keys), tests with a synthetic library (dates across years, a trip far from home, a named person, bursts, screenshots, archived items left out).
2. Refresh thread + API + setting.
3. UI: carousel, Memories page, story viewer.
4. TV row.

### Effort estimate (same basis as above)
| Step | Input tokens (mostly cached) | Output tokens |
|---|---|---|
| 1. Core rules + tests | 1-3M | 12-20k |
| 2. Refresh + API | 1-2M | 5-8k |
| 3. UI (carousel, page, story viewer) | 2-3M | 15-25k |
| 4. TV row | 0.5-1M | 3-5k |
| **Total** | **~5-9M** | **~35-58k** (about 1-2 days) |
