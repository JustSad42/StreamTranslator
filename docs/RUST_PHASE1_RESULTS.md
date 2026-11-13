# Rust rewrite: Phase 1 results (2026-11-12)

Phase 1 = the clip pipeline as a command-line tool (`rust/apps/st-cli`, `streamtranslator-cli.exe`), with the same
arguments, stages, log lines and output files as `python -m streamtranslator.cli`. Code layout and build steps:
`rust/README.md`.

## Results on the reference clip (Twitch VOD 2923514362, 1:00:00–1:03:00, Spanish)

| Run | Python | Rust | Notes |
|---|---|---|---|
| Subtitles only (two-step) | 39 s | 40 s | same 78 lines, same glossary/slang counts, same champions/players read off the video |
| Full job: 720p video + voice isolation + speakers + burn-in | 65 s | 59 s | same 77 lines, 2 speakers, same measured video offset (−1.237 s), burned frames look identical |
| Whisper-only + original subtitles | not compared | 43 s | runs; reuses cached audio and details like Python |

Output files:
- `job.json`, `download.json`, `details.json`: same format as Python (key order, number formatting, Windows line endings);
  only timestamps differ, and champion scores in the 5th decimal.
- Subtitles/transcript: 75 of 78 lines word-for-word identical without isolation. The rest differ by a comma or a word
  ("teleporta" vs "teleporto"), from tiny feature differences (~0.00002) inside Whisper. Given the same audio, Rust and
  Python Whisper agree on 76/77 lines, the same as Python agrees with itself across slightly different audio.
- The full-job run differed in more lines because **Python orders the on-screen player names randomly** between runs
  (it lists them from a Python set), and those names go into Whisper's prompt. Rust keeps first-seen order, so it is
  repeatable. Not a porting bug.

Tests:
- 51 of the 66 Python test cases belong to this phase; all pass (`cargo test -p st-core`, 25 test functions). The other
  15 (chat filters, app updates, problem reports, update helper) move with their features in Phases 2–3.
- Differential check against the Python code with the real glossary files: 400/400 slang preparations, 700/700 clean-ups
  (incl. 300 deliberately misspelled champion names), hotwords, and byte-identical SRT files (speaker colours, time shift).
- Voice isolation on the full clip: 136 dB SNR vs Python, identical speech detection.

## Decisions made in this phase

- **yt-dlp:** ship the official folder build (`yt-dlp_win.zip`). The single-file `yt-dlp.exe` unpacks itself on every
  run: 2.9 s per VOD lookup vs 1.6 s for the folder build (Python: 1.7 s).
- **C runtime:** the whole workspace links the static C runtime (CTranslate2 requires it); ONNX Runtime is loaded from
  `onnxruntime.dll` (Microsoft's DirectML build 1.24.4, same version as Python) at runtime.
- **cuDNN is delay-loaded** by the exe; GPU use is checked first (`gpu::cuda_ready`), so a PC without the GPU libraries
  falls back to the CPU instead of failing to start.
- **Models:** reused from the shared Hugging Face cache; missing ones download into the same layout Python uses.
- Python's `difflib`, `textwrap`, `json.dumps` and text-file newlines were re-implemented so outputs match byte for byte.

## Not tested yet

- YouTube and Kick sources (code ported, including Kick's API fallback, but only Twitch was run).
- CPU fallback on a PC without NVIDIA libraries; turbo/medium/small and custom Whisper models; model downloads from
  scratch (all models were already on disk).
- The `libx264` fallback when NVENC fails; cancelling mid-job.
- Languages other than Spanish; languages NLLB doesn't cover (falls back to Whisper translate).
- "Snap to game" search (`find_game`) and channel VOD listing / "Sync from my VOD": the logic and its tests are ported,
  the UI-driven parts come with Phase 2.

## Phase 2 plan: the Tauri app

1. App shell: Tauri 2 window loading `streamtranslator/ui/` with the QWebChannel shim from the spike; single-instance
   mutex `StreamTranslatorAppMutex`; settings.json / history.json read and written with unknown keys preserved.
2. Bridge: the `api(method)` calls `app.js` makes (about 60 methods) mapped to Rust, events pushed over Tauri events.
3. Clip tab: VOD info, channel VOD list, timeline frames and storyboards, sync, snap-to-game, details, job queue with
   cancel and notifications.
4. Subtitle editor (video plays straight from the .mp4 in WebView2, no WebM preview), history, glossary tab, settings,
   setup screen (GPU libraries, models, ffmpeg), logs, problem reports, crash flag.
5. Port the remaining tests that belong to these features (app updates, reports, update helper).
6. Measure startup time and memory against the Python app.
