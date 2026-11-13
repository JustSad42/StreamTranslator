# StreamTranslator 3 (Rust)

The Rust rewrite of StreamTranslator. The Python app in `../streamtranslator/` stays the reference and keeps
shipping until 3.0.0. Plan and results: `../docs/RUST_REWRITE_HANDOFF.md`, `../docs/RUST_PHASE0_RESULTS.md` … `RUST_PHASE4_RESULTS.md`.

## Layout

| Crate | What it does |
|---|---|
| `crates/st-core` | Pure logic: time formats, subtitles/transcripts, glossary, VOD links and sync maths, OCR text rules, speaker grouping, audio offset, Python-compatible JSON/textwrap/difflib. No GPU, no network. |
| `crates/st-media` | yt-dlp (VOD info, channel VODs, Kick fallback) and ffmpeg (downloads, decoding, frames, burn-in). |
| `crates/st-ml` | Whisper and NLLB (CTranslate2 on CUDA), Silero VAD, voice isolation, speaker fingerprints (ONNX Runtime). |
| `crates/st-vision` | Champion portraits (FFT template matching) and Windows OCR (player names). |
| `crates/st-job` | A clip job start to finish (`run_job`), model cache. |
| `crates/st-appkit` | App plumbing: settings, history, logs and crash detection, app updates, setup-screen downloads, yt-dlp updater. |
| `crates/st-live` | Live mode: stream audio to captions, Twitch chat translation, the OBS browser source. |
| `apps/st-app` | `StreamTranslator.exe`: the Tauri window showing `../streamtranslator/ui` unchanged, its backend, and the floating caption window. |
| `ui-tests/` | End-to-end tests driving the real app (`smoke.mjs` for clips and tabs, `live.mjs` for live mode). |
| `apps/st-cli` | `streamtranslator-cli.exe`, same arguments as `python -m streamtranslator.cli`. |
| `installer/` | Inno Setup script for the 3.0.0 installer (built by `build-installer.ps1`). |

## Building

Needs Visual Studio 2022 Build Tools (C++) and, to compile CTranslate2 with CUDA, NVIDIA's CUDA 12.9 redistributable
zips (cuda_nvcc, cuda_cudart, cuda_cccl, libcublas, libcurand, cuda_profiler_api, cuda_nvtx, cudnn 9 for CUDA 12)
unpacked into `%LOCALAPPDATA%\cuda-redist\12.9` (or set `CUDA_REDIST`). That's only needed to build, not to run.

```bat
cargo.bat build --release -p st-cli
```

`cargo.bat` sets up the compiler environment and runs cargo. It builds for RTX 40-series only (`CUDA_ARCH_LIST=8.9`)
to keep development builds fast; release builds must set a wider list. Crates without the GPU parts (`st-core`,
`st-media`, `st-vision`) build and test with plain `cargo`.

Everything links the static C runtime (CTranslate2 requires it, see `.cargo/config.toml`), so ONNX Runtime is loaded
from `onnxruntime.dll` at runtime instead of being linked.

## Installer

```bat
powershell -ExecutionPolicy Bypass -File build-installer.ps1        :: dist\StreamTranslator-Setup-<version>.exe
powershell -ExecutionPolicy Bypass -File build-installer.ps1 -Test  :: a separate "StreamTranslator Test" app
```

Builds into `target-dist` for every supported GPU (GTX 10 to RTX 50; the first build takes ~10 minutes), downloads the
pinned third-party files into `vendor\dl` (checksums verified), collects everything in `dist\StreamTranslator`, runs the app's
`--selftest`, then compiles `installer\StreamTranslator.iss` with Inno Setup 6. The installer keeps the Python app's AppId,
install folder, mutex and file name, so installed 2.x copies update to it with their own updater. `-Test` builds an installer
with its own AppId and folder, for trying install and uninstall without touching a real installation.

## Running

Next to the exe (the installer will put them there):
- `onnxruntime.dll` (Microsoft.ML.OnnxRuntime.DirectML 1.24.4) and `DirectML.dll` (Microsoft.AI.DirectML 1.15.4)
- `silero_vad_v6.onnx` (from faster-whisper)
- `yt-dlp\yt-dlp.exe`: the official folder build (`yt-dlp_win.zip`); the single-file exe takes 1.5 s just to start
- `ffmpeg\ffmpeg.exe` (or ffmpeg on PATH)

`build-installer.ps1` downloads these into `vendor\dl` (git-ignored) and collects them in `dist\StreamTranslator`; for a
development build, copy them from there next to `target\release\streamtranslator-cli.exe`.

GPU libraries (cuBLAS 12, cuDNN 9) are the ones the Python app already downloads to
`%LOCALAPPDATA%\StreamTranslator\cuda\`. Whisper and NLLB models come from the shared Hugging Face cache.

```bat
target\release\streamtranslator-cli.exe https://www.twitch.tv/videos/2923514362 --start 1:00:00 --end 1:03:00 --no-burn
```

## Tests

```bat
cargo test -p st-core
```

`crates/st-core/tests/test_core.rs` is the port of `tests/test_core.py` (the cases that belong to the clip pipeline).
`crates/st-core/examples/diffcheck.rs` compares the glossary and subtitle writer against the Python app on the same inputs.
