# Plan: Offline "Google Photos"-style app for Windows (Rust + Tauri) with TV streaming

## Context
The project at `I:\My Learnings\Rust\photos` is a fresh `cargo new` (empty `main.rs`, edition 2024, no deps).
Goal: a local, offline-only Windows desktop photo/video library that looks and feels like Google Photos web,
and can stream photos/videos to a TV over the home network.

Decisions confirmed with the user:
- **UI**: Tauri 2 desktop app (Rust backend + web UI).
- **Library**: watch existing folders, index in place, never move files.
- **TV**: DLNA/UPnP media server (TV browses the PC with its remote) **and** a TV-friendly web page on the LAN.
- **Extras**: map/places (GPS), offline face grouping, offline object/scene search (CLIP), duplicate detection.
- Toolchain present: Rust 1.93, Node 24 / npm 11, tauri-cli 2.9.6, FFmpeg/ffprobe 7.1 on PATH.

Offline rule: the app makes **no internet requests**. ML models and map data are bundled with the app
(or placed by the user in the models folder); nothing is downloaded at runtime.

## Architecture
One Rust process (the Tauri app) hosts:
1. **Tauri window** running the web UI (Svelte 5 + TypeScript + Vite).
2. **Embedded axum HTTP server**, two listeners:
   - `127.0.0.1:<port>`: full JSON API + media for the desktop UI (the UI calls it with `fetch`; video `<video src>` uses HTTP range requests). Protected by a random per-launch token passed to the UI.
   - `0.0.0.0:<lan-port>` (off by default, toggle in Settings): **read-only** media, `/tv` web UI, and DLNA endpoints. Optional PIN for `/tv`. Trashed items are never exposed.
3. **SSDP responder** (UDP multicast 239.255.255.250:1900) advertising the DLNA MediaServer while LAN sharing is on.
4. **Background job workers** (tokio + rayon) for scanning, thumbnails, hashing, faces, CLIP.

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
- Tauri: `tauri` 2, `tauri-plugin-dialog`, `tauri-plugin-single-instance`
- Server: `axum`, `tokio`, `tower-http` (ServeFile with Range, CORS for the localhost UI), `quick-xml` (DLNA SOAP/DIDL), `socket2` (multicast), `local-ip-address`, `uuid`
- DB: `rusqlite` (feature `bundled`, with FTS5) + `r2d2_sqlite` pool, `refinery` or hand-written migrations
- Indexing: `walkdir`, `notify` + `notify-debouncer-full`, `blake3`, `kamadak-exif`, `chrono`
- Images: `image`, `fast_image_resize`, `image_hasher` (perceptual hash); HEIC/HEIF and RAW previews via FFmpeg fallback
- Video: ffprobe/ffmpeg invoked as subprocesses (bundled as a Tauri sidecar for the installer; PATH used in dev)
- Geo: `reverse_geocoder` (embedded GeoNames cities, fully offline)
- ML: `ort` (ONNX Runtime, DirectML execution provider for GPU, CPU fallback), `ndarray`, `tokenizers` (CLIP BPE)
- UI: Svelte 5, Vite, TypeScript, MapLibre GL JS (offline, no tile server), a virtualized justified-layout grid

## Data model (SQLite at `%LOCALAPPDATA%\Memora\library.db`)
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
- **Utilities**: Duplicates review (exact + near), Trash (30-day soft delete; "empty trash" sends files to the Windows Recycle Bin via the `trash` crate, never a permanent delete).
- **Settings**: watched folders, LAN sharing toggle + PIN + device name, transcoding on/off, ML on/off and GPU/CPU choice, cache location.
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
- Virtual tree: `Timeline/<Year>/<Month>`, `Albums/<name>`, `People/<name>`, `Places/<country>/<city>`, `Favorites`, `Videos`, `Folders/...`.
- DIDL-Lite items carry `res` entries with `protocolInfo` + DLNA.ORG_PN/OP/FLAGS, a thumbnail `res` (JPEG_TN) and `upnp:albumArtURI`; videos get both the original and a transcoded `res`.
- Responses send `transferMode.dlna.org` and `contentFeatures.dlna.org` headers; honour `Range` and `TimeSeekRange.dlna.org` (seek for transcoded streams via `ffmpeg -ss`).

### TV web page (`/tv` on the LAN listener)
- Separate Vite entry, large tiles, D-pad/arrow-key focus navigation, Enter to open, Back to return.
- Sections: Recent, Albums, People, Places, Favorites, Videos; fullscreen viewer and slideshow with an interval setting.
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

## Verification
- `cargo test --workspace`: unit tests for EXIF/date parsing, DIDL-Lite/SOAP generation (against sample TV requests), SSDP response format, phash/BK-tree, clustering, Range header handling.
- `cargo tauri dev`: add a test folder with JPEG/HEIC/PNG/MP4/MOV/MKV samples (some with GPS, faces, duplicates); confirm grid, viewer, video seek, albums, trash, and that new files dropped in the folder appear without restarting.
- LAN/TV: enable sharing, then
  - open `http://<pc-ip>:<port>/tv` on a phone and on the TV browser, navigate with arrow keys;
  - confirm the PC shows up as a media server in the TV's source list (and in VLC → Local Network → UPnP as a quick check on the PC); browse Timeline/Albums, play a photo, an MP4, and an HEVC/MKV through the transcoded stream, and seek.
- Offline check: run with the network adapter's internet disabled (LAN only) and confirm every feature works; check the app makes no outbound connections (Resource Monitor).
- AI: search "beach", "dog", "car" on a sample set; confirm the same person's faces cluster together and merge/rename persists.

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

## Progress (updated 2026-11-05)

**Status: Phases 1–5 built (5 of 7). Phase 5 (TV) was driven in the running app over the LAN address; it still needs a test on the real Samsung TV. Next: Phase 6 (AI).**

| Phase | Status |
|---|---|
| 1. Skeleton | ✅ Done |
| 2. Library core | ✅ Done |
| 3. Organize | ✅ Done |
| 4. Places & duplicates | ✅ Done |
| 5. TV (DLNA + /tv) | ✅ Built, awaiting a real-TV test |
| 6. AI (faces + CLIP) | ⏳ Next |
| 7. Packaging | Not started |

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

Changes from the original plan:
- No `jobs` table: pending work is the `media.stage` column (0 new → 1 metadata → 2 thumbnail). Later phases add stages.
- Thumbnails are JPEG (400 px tall, max 1600 wide), cached by media id at `cache/thumb/<id/1000>/<id>.jpg`, so renames keep them. JPEG instead of WebP because the `image` crate only writes lossless WebP, and DLNA needs JPEG thumbnails anyway.
- `blake3` hashing moved to Phase 4 (duplicates).
- Album items are sorted by date like every other grid; there is no manual item order (the day-grouped grid needs date order). Albums themselves are listed newest first.
- FTS covers file/folder names, camera, album and place names; people and scene search arrive with Phase 6.
- TV page is plain TypeScript, not Svelte (old TV browsers). DLNA has no `Search` action (TVs browse). No People folder yet (Phase 6).
- No street-level map: the offline basemap stops at cities. An optional `.pmtiles` detailed basemap is not built. No "not a duplicate" memory for false positives yet.

### Remaining

**Phase 6: AI**
- `ort` with DirectML/CPU; YuNet + SFace faces, clustering, People page (rename/merge/hide).
- CLIP image/text embeddings and semantic search.

**Phase 7: Packaging**
- Bundle FFmpeg as a sidecar, models and GeoJSON; NSIS/MSI installer; firewall rule for the LAN port.

### Open issues / to verify
- HEIC rotation is verified only on OnePlus photos (pixels stored upright). iPhone HEICs (pixels stored sideways + rotation flag) still need a check.
- RAW camera files (CR2, NEF, ARW, DNG…) are not indexed yet (`I:\My Photos` has 7 DNG files).
- First-time indexing of a big HEIC library takes a while (thumbnails ~3+ per second in dev builds); the grid itself fills in about a minute.
- Videos in codecs the webview can't play now play as a converted copy (skipping ahead is limited there); "Open in default player" stays as a fallback.
- **Test on the Samsung TV**: the source list entry, browsing, photos, an HEVC phone video (original), an MKV/odd file (converted), seeking, and the `/tv` page in the TV browser (which may use a pointer instead of arrow keys). A device at 192.168.0.171 found and browsed the server during testing (probably the TV).
- HDR phone videos are converted to 8-bit without tone mapping, so colours may look washed out on the converted stream (originals are fine).
- DLNA can't have a PIN; anyone on the Wi-Fi can browse while it's on (said in Settings).
- The installer must add a Windows Firewall rule (TCP port + UDP 1900) in Phase 7; in dev, Windows asks the first time.
- Video dates from `creation_time` are treated as UTC; some cameras write local time there, which shifts them by the timezone offset.
- Nothing is committed to git yet.
- Not exercised in the running app yet: the viewer's actions (favorite, trash, add to album, use as cover), the album picker and album pages, Empty trash / Delete forever (needs a real Recycle Bin run), the search filters (type, camera, dates), light theme, and the Places detail page and map in a production build (`cargo tauri build`).
- Svelte logs a dev-only warning for `library.status ??= s` in `lib/library.svelte.ts` (`assignment_value_stale`); harmless, worth tidying.
- Emptying the trash on a removable drive without a Recycle Bin is untested.
- The first run after the Phase 4 upgrade reads every file once (blake3), so indexing shows "Indexing… N left" for a while on a big library.
- The `I:` drive is nearly full (3.9 GB free on 2026-11-05). Build output now goes to `C:/Users/Akash/AppData/Local/MemoraBuild/target` (set in `.cargo/config.toml`); the old `target/` folder in this project (12+ GB) is unused and can be deleted.

---

## Planned: Private vault ("Locked folder")  (added 2026-11-05, not started)

Goal: private photos and videos are **encrypted on disk** and open only inside this app after a password. Hiding by attribute or folder name is not enough; files must be unreadable from Explorer or other apps. The vault is never exposed on the LAN, DLNA or `/tv`.

### Design
**Storage**
- Vault folder (configurable, default `%LOCALAPPDATA%\Memora\vault\ab\<random-id>.enc`): random names, no extension, originals removed.
- Metadata (original name, dates, EXIF, GPS, dimensions) in a separate **SQLCipher** database `vault.db`, never in `library.db` or `media_fts`.
- Thumbnails and the JPEG preview of HEIC are encrypted too. They are rendered at import time (while the original is readable), so later viewing never needs plaintext. Tiled-HEIC stitching needs file paths for FFmpeg, which is another reason to render at import.

**Crypto** (audited crates only: `argon2`, `chacha20poly1305`, `zeroize`, `rusqlite` with SQLCipher)
- Password -> **Argon2id** -> key-encryption key that wraps a random 256-bit **master key**. Changing the password only re-wraps the master key.
- **Recovery key** (random, shown once) also wraps the master key. Forgotten password without it = data lost.
- Each file has its own random file key, wrapped by the master key.
- Files are encrypted in 64 KB chunks with **XChaCha20-Poly1305**; chunk index and a "final chunk" flag are in the authenticated data (stops truncation and reordering). Chunking keeps HTTP Range working, so video seeking still works.
- Versioned file header so the format can change later.

**Access**
- Unlock derives the key and keeps it in memory only (`zeroize`). Auto-lock on idle, minimize or Windows lock; the key is wiped.
- Vault endpoints exist only on the localhost API and need an unlocked session (token). They are not routed on the LAN/DLNA listener.
- Vault items are excluded from timeline, search, map, places, duplicates, faces, CLIP, trash, albums, favorites, DLNA and `/tv`.
- "Open in default player" and "Show in Explorer" are disabled for vault items.
- Later, optional: Windows Hello unlock; exclude the window from screen capture (`WDA_EXCLUDEFROMCAPTURE`).

**"Move to locked folder" flow**
1. Select items -> action -> password (unlock if locked).
2. Per item: extract metadata, make thumbnails/preview, encrypt, then **decrypt and compare the hash**.
3. Only after verification delete the original (permanent delete, **not** the Recycle Bin, which would keep a plaintext copy).
4. Purge everything derived from it: `media` row, `cache/thumb`, `cache/tv`, `cache/tn`, FTS entry, album/favorite links, place, face and CLIP data. Remove the row before the file so the watcher does not treat it as "missing".
5. "Remove from vault" decrypts back to a folder the user picks.

### Caveats
- This **breaks the "index in place, never move files" rule** for vault items; the UI must say so.
- Deleting the original on an SSD does not guarantee the data is wiped (TRIM/remnants). Recommended companion: **BitLocker / BitLocker To Go** on the drives (free on Windows 11 Pro). Layers: BitLocker protects a stolen or lost drive; the vault protects against anyone using the logged-in PC or other apps. A VeraCrypt container is an alternative without code, but integrates less cleanly.
- Malware on a PC while the vault is unlocked can read what is being viewed; auto-lock reduces this but cannot remove it.
- The `I:` drive is nearly full, so the vault location must be configurable.
- Encrypted videos cannot be played by external players.
- Hibernation file and page file may hold key material; BitLocker covers this.

### Steps
1. Crypto module (Argon2id, key wrapping, chunked AEAD) with tests: round trip, tamper/truncate/reorder detection, wrong password, seek to any offset.
2. `vault.db` (SQLCipher), setup/unlock/lock/change password/recovery, import and export with verify-then-delete and cache purge.
3. Localhost streaming: decrypt on the fly with Range, encrypted thumbnails/previews, unlocked-session check.
4. UI: Locked folder page, setup (password + recovery key), unlock screen, auto-lock, "Move to locked folder" in the selection bar and viewer.
5. Exclusion from every other feature (timeline, search, map, duplicates, ML, TV, DLNA) plus integration tests (e.g. vault item never returned by `/api/timeline`, LAN listener returns 404 for it).

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

### Open decisions
- Vault location: configurable folder, default `%LOCALAPPDATA%\Memora\vault` (suggested).
- First version: password + recovery key only; Windows Hello later (suggested).
