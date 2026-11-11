# Third-party software and data in Memora

Memora itself is MIT-licensed (see `LICENSE`). The Windows installer also contains the
programs, models and data below, each under its own license.

## Programs shipped next to memora.exe

- **FFmpeg 7.1.1** (`ffmpeg.exe`, `ffprobe.exe`), "essentials" build by Gyan Doshi
  (www.gyan.dev). Licensed under the GNU General Public License, version 3 (`FFmpeg-LICENSE.txt`);
  build details in `FFmpeg-README.txt`. FFmpeg runs as a separate program; Memora starts it to
  read and convert videos and HEIC photos.
  Source code: https://github.com/FFmpeg/FFmpeg/commit/db69d06eee (FFmpeg 7.1.1), and the build
  scripts and libraries listed at https://www.gyan.dev/ffmpeg/builds/. You may also ask the
  Memora maintainers for a copy of the corresponding source code, for at least three years after
  this release.
- **DirectML** (`DirectML.dll`), Copyright (c) Microsoft Corporation, redistributed under the
  Microsoft DirectML license.

## Recognition models (`models\`)

- YuNet face detector and SFace face recognizer, from the OpenCV Model Zoo: Apache License 2.0.
- CLIP ViT-B/32 (OpenAI), ONNX export by Xenova (transformers.js): MIT License.

## Built into memora.exe

- ONNX Runtime (Microsoft): MIT License.
- SQLite: public domain.
- Time zone boundaries from timezone-boundary-builder (via `tzf-rs`): data © OpenStreetMap
  contributors, available under the Open Database License (ODbL 1.0).
- City names and positions from GeoNames (via `reverse_geocoder`): Creative Commons
  Attribution 4.0.
- Map data from Natural Earth (public domain) and Noto Sans glyphs (SIL Open Font License 1.1).
- The JPEG encoder (`jpeg-encoder`) is based in part on the work of the Independent JPEG Group.
- Many Rust and JavaScript libraries under the MIT, Apache-2.0, BSD and similar licenses; see
  `Cargo.lock` and `ui/package-lock.json` in the source code for the full list.
