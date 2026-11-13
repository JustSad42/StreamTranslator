# Rust rewrite: Phase 0 spike results (2026-11-12)

All spikes ran on the user's PC (RTX 4080, Windows 11) against the Python 2.3.1 code on the same inputs:
a 3-minute Spanish clip (Twitch VOD 2923514362, 1:00:00–1:03:00, Ibai co-stream) and the 4 frames the
Python job grabbed from it. Spike code: `rust/spikes/*`. Python reference scripts and outputs were kept in the
session scratchpad (not in the repo).

## Results

| Area | Rust library | Matches Python? | Speed, Rust vs Python |
|---|---|---|---|
| Whisper large-v3 (CUDA, float16) | `ct2rs-sys` 0.11 (CTranslate2 4.8.5, built with CUDA 12.9) + own port of faster-whisper's loop | 75/78 lines word-for-word identical, other 3 differ by one word or comma; 76/78 timings identical; VAD chunks and mel features identical | transcribe 17.4 s vs 17.5 s (same); model load 2.7 s vs 3.9 s; language detect 0.4 s vs 0.7 s |
| NLLB 1.3B (token batching) | `ct2rs-sys` Translator + `sentencepiece` | 400/400 sentences identical | 2.72 s vs 2.56 s (6% slower) |
| Voice isolation (UVR, DirectML) | `ort` 2.0.0-rc.15 + `realfft` | identical output (134 dB SNR) | 75× realtime vs 31× |
| Speaker fingerprints (WeSpeaker) | `ort` (CPU) | identical (cosine 1.000000) | 1.6 s vs 1.3 s for 65 lines (slower; DirectML 2.9 s) |
| Champion icons (TM_CCOEFF_NORMED) | hand-written FFT matcher, `rustfft` + `rayon` | same top 10, same scores to 4 decimals; resize pixel-identical to OpenCV | 7.0 s vs 10.4 s |
| Windows OCR | `windows` 0.62 (WinRT) on one dedicated thread | identical text on 4/4 frames; 16 parallel callers OK | 0.37 s vs 0.35 s |
| UI | Tauri 2.12 (WebView2) | existing `ui/` runs unchanged via a ~15-line QWebChannel shim | page ready 0.5 s after launch; window shown 1.2 s vs 1.3 s |

Sizes: Tauri shell exe 9 MB; Whisper+NLLB exe 16 MB (one GPU architecture); Python install is 1.1 GB
(PySide6 458 MB, bundled ffmpeg 218 MB, OpenCV 112 MB). Rough Rust install estimate: ~350 MB with the
same ffmpeg, mostly ffmpeg.

## Findings that change the plan

1. **CTranslate2 must be compiled by us.** `ct2rs` builds it from source; for CUDA that needs NVIDIA's compiler at
   build time only (not on users' PCs). I used NVIDIA's official redistributable zips extracted to
   `%LOCALAPPDATA%\cuda-redist\12.9` (no system install). `rust/build-ct2.bat` sets this up. At runtime the app
   reuses the cuBLAS/cuDNN DLLs it already downloads to `%LOCALAPPDATA%\StreamTranslator\cuda\`. Release builds
   must compile for several GPU generations (spike: RTX 40 only), which makes the build slower and the exe bigger.
2. **C runtime clash.** `ct2rs` forces the static C runtime; `ort`'s prebuilt ONNX Runtime uses the dynamic one, so
   they can't link into one exe. Fix (verified): `ort` with `load-dynamic`, shipping Microsoft's DirectML build of
   `onnxruntime.dll` (~21 MB) + `DirectML.dll` next to the exe.
3. **cuDNN is linked at load time** by CTranslate2, so the exe needs a delay-load of `cudnn64_9.dll` plus
   `AddDllDirectory` to the app's cuda folder before the first GPU call (the spike used PATH).
4. **First run after a fresh build** had a one-off 10 s delay on the first GPU call (gone on the next run). Check on a
   clean install in Phase 4.
5. `ct2rs` vendors CTranslate2 4.8.5 (Python uses 4.9.0). Output was still identical; revisit if a 4.9 fix matters.

## Not tested yet

Video playback in WebView2 (the editor), the always-on-top caption window, 4K DPI behaviour, live mode, CPU fallback
(machines without NVIDIA), turbo/medium/small Whisper models, Whisper-only translate mode, Kick/YouTube sources.

## Recommendation

Go ahead with: `ct2rs-sys` (low-level API) for Whisper and NLLB with our own port of faster-whisper's decoding loop;
`ort` (load-dynamic) + DirectML; `windows` crate for OCR; the hand-written FFT matcher; Tauri 2 with the existing UI.
No need to try `whisper-rs`: CTranslate2 from Rust already matches Python's text and speed.

## Phase 1 plan (core pipeline as a CLI)

Workspace layout in `rust/`:
- `crates/core`: settings/glossary/history files (unknown keys preserved), time formats, SRT/transcript writers,
  glossary fixes + slang + fuzzy names, job.json/download.json/details.json.
- `crates/media`: ffmpeg runner (progress, cancel, `CREATE_NO_WINDOW`), bundled `yt-dlp.exe` (`-J`), Kick fallback,
  section download, audio decode, FFT audio offset.
- `crates/asr`: Silero VAD, mel features, Whisper loop (fallback, no-speech, junk/echo/repeat filters), language
  detection (8 segments, 0.7), NLLB with sentence split; GPU DLL setup (delay-load + DLL dirs), CPU int8 fallback.
- `crates/vision`: champion matcher + OCR thread + players/nameplates, snap-to-game clock voting.
- `crates/audio`: UVR isolation, WeSpeaker + clustering, speaker colours.
- `apps/cli`: same arguments as `python -m streamtranslator.cli`.

Steps: port the 66 tests first (they're the spec for the fiddly parts), then the modules above, then run the CLI on
the reference clip and diff its `.en.srt`, `.es.srt`, transcript and `job.json` against Python's, and time the whole
job against Python's 39 s. Also: release build script for multiple GPU architectures, and pick the Microsoft
`onnxruntime.dll` (DirectML) version to bundle.
