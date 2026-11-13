# Rust rewrite: Phase 2 results (2026-11-12)

Phase 2 = the desktop app: a Tauri (WebView2) window showing the existing interface in `streamtranslator/ui`, unchanged,
plus a Rust backend that answers every call the interface makes, with the same result shapes and events as the Python
backend (`app.py`). Code: `rust/apps/st-app` (window + backend) and `rust/crates/st-appkit` (settings, history, logs and
crash detection, app updates, setup-screen checks and downloads, champion list refresh, yt-dlp updater).

## How it talks to the interface

The interface was written for Qt's QWebChannel (`backend.request(json)` plus a `backend.event` signal). A small script
injected at page load recreates that object on top of Tauri's invoke/events, so `app.js` runs as it is. Same for the
Ctrl+= / Ctrl+- / Ctrl+0 interface-size keys. Local video and images are served through Tauri's asset protocol.

## End-to-end UI test (`rust/ui-tests/smoke.mjs`)

Drives the real app through the WebView2 debugging port (only enabled when `ST_DEVTOOLS_PORT` is set), using a copy of the
user's settings/history/glossary. **19/19 steps pass**, no errors in the page console, app closes normally:

| Step | Result |
|---|---|
| Interface boots, model chip turns "ready on RTX 4080" | 4.6 s incl. Whisper large-v3 preload |
| Glossary test box, setup-screen component status | OK |
| Channel lookup "ibai" | 12 VODs in 1.2 s |
| Load a VOD through the Clip tab (timeline storyboard, frame grab) | OK (5.1 s, 0.3 s) |
| Sync from my VOD (same VOD → offset 0; unknown channel → friendly error) | OK |
| Snap to game (champion icons + OCR on the VOD) | game 2733–4451 s found in 40 s |
| 30 s clip through the queue, 720p, burned subtitles | 12 lines in 20 s, all 8 stages reported |
| History lists the clip, full-text search finds it | OK |
| Subtitle editor plays the clip (H.264 directly, no WebM preview step) | 1280×720, 28.8 s, 12 cues |
| Editor save keeps `.original.srt`; re-burn edited subtitles | OK (3.6 s) |
| Glossary tab save/restore custom word; slang "try it" (loads NLLB) | OK |
| Settings + interface size shortcuts | 1.2 → 1.3 → back |
| Live tab | says politely that it comes in the next phase |

Bug found and fixed by this test: the first clip job froze because the settings lock was taken twice (deadlock).

## Compared with the Python app (same PC, same settings copy, 20 s after launch)

| | Python 2.3.1 (installed) | Rust |
|---|---|---|
| Window visible | 3.4 s first launch, 0.9 s second | 0.5 s both times |
| Memory (working set, all processes) | ~575 MB | ~635 MB |
| Memory (private, includes CUDA + Whisper on GPU) | ~5.4 GB | ~4.75 GB |
| Install folder (without GPU libraries/models, downloaded by both) | 1.1 GB | ~110 MB (exe 32 MB, ONNX Runtime + DirectML 35 MB, yt-dlp 40 MB) |

Memory is about the same: most of it is Whisper and CUDA. The Python working-set number may undercount Qt WebEngine's
helper processes.

## Tests

- `cargo test -p st-core -p st-appkit`: all pass. 56 of the 66 Python test cases are now ported; the other 10 are chat
  filters, which move with live mode in Phase 3.
- `st-appkit` tests include the real update helper running and replacing a file, checksum rejection, version comparison,
  release parsing, problem-report links, and settings keeping unknown keys (so Python and Rust can share one settings file).

## Not tested yet

- Setup screen downloads from scratch (GPU libraries, models) on a clean PC; CPU fallback on a PC without NVIDIA.
- App update install end-to-end (download → installer → restart); comes with the 3.0.0 installer in Phase 4.
- Windows notifications show the right app name/icon (needs the installed shortcut; Phase 4).
- Tray icon menu, dragging a link onto the window, YouTube and Kick sources, cancel in the middle of each stage.
- Python's clip job was not timed through its own UI on the same 30 s clip (Phase 1 already compared the pipeline).

## For Phase 4 (packaging)

- `target/release/DirectML.dll` is a symlink created by ort's build; the installer must copy the real file
  (`rust/vendor/bin/DirectML.dll`).
- Ship the yt-dlp folder build, `onnxruntime.dll`, `DirectML.dll`, `silero_vad_v6.onnx` next to the exe.
