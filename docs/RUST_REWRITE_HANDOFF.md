# StreamTranslator: Rust rewrite handoff

You are taking over a working Windows desktop app, StreamTranslator 2.3.1, and rewriting it in Rust.
The Python version stays the reference: same features, same files on disk, same results.
This document is what the previous session learned building it. Read it fully before writing code.

- Repo: this folder (public, https://github.com/JustSad42/StreamTranslator, branch `main`)
- Python source: `streamtranslator/` (about 4,300 lines of Python, plus 2,100 of HTML/JS/CSS in `streamtranslator/ui/`)
- Run the Python app from source: `.venv\Scripts\python -m streamtranslator.app` (or double-click `StreamTranslator.pyw`)
- Tests: `.venv\Scripts\python -m pytest -q` (66 tests in `tests/test_core.py`; port them, they are the spec for the fiddly parts)
- Installed copy: `%LOCALAPPDATA%\Programs\StreamTranslator` (Inno Setup, per-user). Releases: `release.ps1 -Version x.y.z -Notes file.md`
- The user has an RTX 4080, Windows 11, a 4K monitor at 100% Windows scaling. They play and stream League of Legends.

## Set expectations first

The previous session told the user a Rust rewrite would **not make translations faster**: nearly all the time
is spent in CTranslate2 (Whisper, NLLB) on the GPU, ffmpeg, ONNX Runtime and network downloads, which are
already native code. Python's share of a clip job is under 1%. What Rust can realistically improve:

- startup time (Python + Qt WebEngine take a few seconds), memory use, installer size (today 362 MB, 1.08 GB installed)
- one native binary instead of a frozen Python app (fewer packaging hacks)
- a native WebView2 UI (sharp at any DPI, plays H.264, so the editor no longer needs a WebM preview transcode)

The user decided to rewrite anyway. Don't argue it again, but measure each phase against the Python version
and tell them honestly what got better or worse. Do spikes before committing to a library (Phase 0).

## What the app does (feature inventory; all must survive)

**Clip tab: translate a section of a VOD**
- Sources: Twitch VODs, YouTube videos, Kick VODs (yt-dlp; Kick falls back to `https://kick.com/api/v1/video/<uuid>` →
  `source` m3u8 when yt-dlp's Kick extractor fails, which it currently does). Typing a Twitch channel lists its recent VODs.
- Timeline: storyboard filmstrip (Twitch sprite sheets from yt-dlp's `storyboard` formats), hover previews (ffmpeg frame grabs),
  zoom, drag handles, typed times, `?t=` in links.
- **Sync from my VOD**: given the user's VOD and game start/end times, find each other streamer's VOD covering that moment:
  `their_t = my_t + (my_start_ts − their_start_ts)`, padded ±15 s. Twitch VOD start time comes from yt-dlp `timestamp`, or from
  the thumbnail URL (`_(\d{10})/thumb|storyboards`) in flat playlists. Several channels → "Translate all" queues each.
- **Snap to game**: Windows OCR reads the in-game clock (mm:ss) on sampled frames; each reading at VOD time T votes for
  "game started at T − clock"; biggest agreeing cluster (tolerance 3 s) wins; walk forward to find where the clock disappears.
  OCR digit fixes (O→0, l→1, S→5…) and misread-minute variants (`_CONFUSE`). Skips broadcast replays. See `snap.py`.
- **Read champions and players from the video** (`detect.py`): 4 frames; template-match Riot Data Dragon champion icons
  (border cropped 13%, sizes 18–52 px in a 1280-wide frame, TM_CCOEFF_NORMED, threshold 0.80, best score per champion
  over frames, top 10). Speed trick: half-size pass over all champions, then full-size pass on the top 30 (4x faster, same results).
  Player names: OCR nameplates `TEAM Name`, Tl→T1 fixes, HUD-word filter. Results cached per section in `details.json`.
- Options: Whisper model (large-v3 / turbo / medium / small / a custom faster-whisper folder), method (two-step or Whisper-only),
  League glossary, isolate voices, label speakers, burn subtitles, original-language subtitles, quality, subtitle position/size.
- Job queue: one job at a time, cancellable, Windows notifications when the window isn't focused.
- Outputs per job in `<out_dir>\<channel>_<id>_<start>-<end>\`: `.mp4` (video), `.m4a` (audio), `.subbed.mp4`, `.en.srt`,
  `.<lang>.srt`, `.transcript.txt` (VOD + clip timestamps, "Speaker N:" labels), `thumb.jpg`, `job.json` (meta), `download.json`
  (download cache), `details.json` (detection cache).
- **Subtitle editor**: video + cue list; edit text/timing, split, add, delete, set start/end at playhead, save, re-burn. Keeps speaker colours.

**Live tab: any live stream**
- ffmpeg pulls audio from the stream URL (yt-dlp resolves Twitch/YouTube/Kick live links) → 16 kHz PCM pipe → Silero VAD chunking
  (sentence ends after 0.45 s silence, forced cut at 9 s) → draft captions every 1 s, final caption 1–2 s after each sentence.
- Floating always-on-top caption window (drag, scroll to resize), OBS browser source (local HTTP server + Server-Sent Events on
  port 47851, `?size=56&bg=0.4&bottom=8`), transcript + SRT saved to `Videos\StreamTranslator\Live\`, reconnects on drops.
- **Chat translation**: anonymous Twitch IRC (`justinfan…` over TLS 6697), emote stripping, laugh filter ("jajaja", "kkkk", "rsrs"),
  English messages passed through, NLLB translation.

**History tab**: every clip and live session with thumbnails; search titles, channels and transcript text (with timestamps).

**Glossary tab**: Whisper hotwords (champions in the game first, then the user's words, then core terms like Baron/Rift Herald),
post-translation fixes (foreign terms, garbled champion names via exact + fuzzy match with difflib cutoff 0.84, `COMMON` word
guard), user fixes, **slang list** (68 built-in regexes for es/pt/fr/de, swapped for plain English *before* NLLB translates;
user entries any language), "try it" boxes. Champion list from Data Dragon, cached, works offline.

**Settings / app**: interface size, notifications, startup Whisper preload, text translator size (NLLB 1.3B "better" 2.7 GB /
600M "standard" 1.2 GB), first-run setup screen (GPU libraries, models, FFmpeg), yt-dlp auto-update, app auto-update from GitHub
releases with What's new and Go back, Report a problem (pre-filled GitHub issue, user name redacted), logs, crash detection.

## The translation pipeline (clip job), in order

Python: `pipeline.run_job`. Stage names are shown in the UI; keep them.

1. **Reading VOD info**: re-read with yt-dlp every job (Twitch playlist tokens expire).
2. **Downloading audio**: audio-only stream for the section (`-ss start -i url -t dur -c copy` → `.m4a`). Cached via `download.json`
   (file name, size, quality). Meanwhile the **video downloads in the background** (if burning) and detection runs.
3. **Loading Whisper** (CTranslate2, float16 on CUDA, CPU int8 fallback; up to 2 models cached).
4. **Isolating voices** (optional): UVR MDX-Net Voc_FT ONNX on DirectML (n_fft 7680, hop 1024, dim_f 3072, dim_t 256, ~6 s windows),
   loudness-matched, 15% of the original mixed back in. Release the session afterwards to free VRAM for Whisper.
5. **Detecting language** (Whisper, 8 segments, threshold 0.7) unless set manually.
6. **Two-step** (default): Whisper *transcribes* in the source language (hotwords = champions + player names, no English core terms),
   then NLLB translates to English. Then glossary fixes. If NLLB doesn't support the language, fall back to Whisper translate.
   **Whisper-only**: Whisper task=translate (+ optional second transcribe pass for original subtitles).
   Whisper settings: beam 5, VAD on (min silence 500 ms), `condition_on_previous_text=False`, no_speech 0.6. Drop junk lines
   (`_JUNK` phrases, no_speech>0.75 with logprob<−0.9, punctuation-only), drop hotword-list echoes, keep max 2 identical repeats.
7. **Telling speakers apart** (optional, `speakers.py`): WeSpeaker ResNet34 ONNX (26 MB, sha256 `7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068`, HF
   `Wespeaker/wespeaker-voxceleb-resnet34-LM`), input 80-dim Kaldi-style fbank (25 ms/10 ms, povey window, preemphasis 0.97,
   x·32768, mean-normalised), lines ≥0.8 s, centre crop ≤8 s. Average-linkage clustering at 0.3, then merge groups whose
   duration-weighted mean embeddings are ≥0.3 similar, fold tiny groups (<4% time and ≤2 lines). Speaker 1 = most talk time.
   Tuned on real stream audio: single-line similarity is noisy (same person ~0.37, different ~0.02).
8. **Downloading video** (wait for the background download), then **measure the offset** between the transcribed audio and
   the video's audio by FFT cross-correlation of the first minute (±15 s). Twitch video sections start up to several seconds
   off the requested time (keyframes); one test was 1.24 s late, another 5.8 s early. Shift all subtitle times by it.
9. **Burning subtitles**: ffmpeg `subtitles=_burn.srt:force_style=...`, run with cwd = the folder and a relative file name (avoids
   filter path escaping), h264_nvenc (p5, cq 21) with libx264 fallback. Speaker colours via `<font color="#RRGGBB">` in the SRT
   (libass renders them).

NLLB details (`translate.py`): models `JustFrederik/nllb-200-distilled-{600M,1.3B}-ct2-float16`; split each line into sentences first
(NLLB drops later sentences otherwise); one `translate_batch` call with `max_batch_size=4096, batch_type="tokens"` (6x faster than
fixed batches of 32); beam 4, repetition_penalty 1.1, max length 256, `target_prefix=["eng_Latn"]`, source token = NLLB language code.

## Library mapping (verify each in Phase 0)

| Python today | Rust candidate | Notes |
|---|---|---|
| PySide6 + QtWebEngine + QWebChannel | **Tauri 2** (WebView2) | Reuse `ui/index.html`, `style.css`, `app.js` almost as is: swap the `api(method, params)` / `on(event)` bridge for `invoke` + Tauri events. Drop the manual zoom/DPI code; drop the WebM preview transcode. |
| faster-whisper (CTranslate2) | `ct2rs` (CTranslate2 bindings: Whisper + Translator) | faster-whisper adds a lot in Python: Silero VAD + speech-chunk stitching, timestamps, hotwords (prompt prefix), language detection, temperature fallback, no-speech filtering. Budget real time to port it. Alternative: `whisper-rs` (whisper.cpp, CUDA) but quality/speed must match large-v3 on CTranslate2; compare on the same clips. |
| NLLB via ctranslate2 + sentencepiece | `ct2rs` Translator + `sentencepiece` crate | Check token-batching options exist in the binding. |
| onnxruntime-directml (isolation, speakers, Silero VAD) | `ort` with DirectML EP | Isolation STFT/ISTFT with `realfft`. |
| yt-dlp (Python package, auto-updated from PyPI wheels) | **bundled official `yt-dlp.exe`**, run with `-J` / `--dump-json` | No Rust equivalent. Update with its own `-U` or by downloading the release exe (verify SHA2-256SUMS). YouTube may soon need a JS runtime (deno); yt-dlp warns already. |
| ffmpeg subprocess | same | Keep `CREATE_NO_WINDOW`. |
| winrt Windows.Media.Ocr | `windows` crate (WinRT) | Do OCR on one dedicated MTA thread. In Python, calling it from arbitrary worker threads crashed the whole app (access violation) once other native libs were loaded. |
| OpenCV matchTemplate | `imageproc::template_matching` or hand-written NCC, `rayon` | Must reproduce TM_CCOEFF_NORMED scores (threshold 0.80 was tuned on them). |
| urllib, json | `reqwest`, `serde_json` | |
| ThreadingHTTPServer + SSE (OBS) | `axum` or `tiny_http` | |
| IRC socket | `tokio` + `tokio-native-tls` | |
| difflib fuzzy match | `strsim` | Re-check the glossary tests; ratios differ between algorithms. |
| logging + faulthandler | `tracing` + `tracing-appender`, panic hook, minidump optional | |
| PyInstaller + Inno Setup | `cargo build --release` + **keep Inno Setup** (see Migration) | |

## Files on disk the new app must keep using

- `%APPDATA%\StreamTranslator\`: `settings.json`, `glossary.json` (enabled, use_core_terms, custom_words, fixes, use_slang, slang),
  `history.json`, `champions.json`. Read the existing files; unknown keys must survive a save.
- `%LOCALAPPDATA%\StreamTranslator\`: `cuda\*\bin` (downloaded cuBLAS 12.9.2.10 / cuDNN 9.28.1.4 DLLs, or `gpu_libs.json` pointing at an
  existing install), `models\` (UVR + WeSpeaker ONNX), `icons\<ddragon version>\`, `updates\` (installers, `notes-<v>.md`, `install.log`),
  `logs\` (`app.log`, `crash.log`, `running.flag`), `pylib\` (Python-only: downloaded yt-dlp; not needed in Rust).
- Whisper and NLLB models live in the Hugging Face cache (`%USERPROFILE%\.cache\huggingface\hub`); reuse them, don't re-download.
- Settings keys in use: see `DEFAULTS` in `app.py`. History and job `job.json` formats: see `history.py` and `run_job`'s `meta`.

## Migration and updates (important, easy to get wrong)

The installed 2.3.1 Python app updates itself by: GET `https://api.github.com/repos/JustSad42/StreamTranslator/releases/latest`,
pick the asset whose name starts with `StreamTranslator-Setup` and ends `.exe`, verify GitHub's `digest` (sha256), then run it with
**Inno Setup flags** `/SILENT /SUPPRESSMSGBOXES /NORESTART /LOG="..."` from a PowerShell helper launched with `CREATE_NO_WINDOW`
after the app exits. Consequences:

- The first Rust release must be an **Inno Setup installer with the same AppId** (`{4C7E1B92-3A5D-4F8E-B0C6-57A1D9E2F310}`), same asset
  name pattern, same install dir, and must accept those flags. Then existing installs upgrade in place. A Tauri NSIS/MSI installer would
  break this path (NSIS ignores `/SILENT`).
- `installer.iss` already has a `[Run]` entry with `Check: WizardSilent` that reopens the app after a silent update; keep it. Keep `AppMutex`
  (`StreamTranslatorAppMutex`) and create that mutex in the Rust app.
- The Rust app's own updater must keep doing the same (or switch later, deliberately). Keep "What's new" (`last_version`, `updated_from`,
  `skip_version` settings) and "Go back" (installs the previous release by tag).
- Bug learned the hard way: launching the helper with `DETACHED_PROCESS` makes PowerShell silently do nothing, so updates never installed
  (2.2.0 and 2.3.0). Use `CREATE_NO_WINDOW` (+ `CREATE_BREAKAWAY_FROM_JOB` with fallback). Test updates from the installed app, not from a terminal.
- Version the Rust build 3.0.0. Don't publish a release until it reaches feature parity and the user agrees.

## Lessons that cost time (don't relearn them)

- HLS section download with `-ss` before `-i` and `-c copy` snaps to segment/keyframe boundaries; the audio-only rendition is accurate,
  video isn't. Measure and shift (step 8).
- YouTube video formats are video-only (DASH/HLS); the sound is a separate stream: two ffmpeg inputs, map `0:v` + `1:a`. Prefer AAC
  (`mp4a`) audio and non-"DRC" formats. Don't force `aac_adtstoasc`; the mp4 muxer adds it when needed and it breaks Opus.
- yt-dlp's Twitch IDs start with `v`; strip it only for Twitch (YouTube IDs can start with `v`).
- Whisper repeats its hotword prompt during silence; filter lines that are mostly hotwords.
- NLLB takes slang literally ("de menos a más" → "from less to more"); pre-replacing it with English inside the source sentence works well.
- Template matching all ~170 champions at 7 sizes on 4 frames took ~40 s; the shortlist pass fixed it.
- Don't wait more than 20 s for detection; start transcription without it.
- The faster-whisper Python package pulls in CPU-only onnxruntime, which hid the GPU one (irrelevant in Rust, but `ort` must load the DirectML build).
- Idle GPU use should be ~2–3%; animations only while active.
- The user's display: auto scale = clamp(screen_height/1200, 1.0–2.5); WebView2 should make this mostly unnecessary.

## Suggested plan

**Phase 0: spikes (decide libraries, measure against Python on the same clips)**
1. `ct2rs` Whisper large-v3 on CUDA with the downloaded cuBLAS/cuDNN DLLs: transcribe a 3-minute Spanish clip (e.g. Twitch VOD 2923514362,
   1:00:00–1:03:00, Ibai co-stream), compare text and time with faster-whisper. Include VAD and hotwords.
2. `ct2rs` NLLB 1.3B with token batching: 400 sentences under ~2 s.
3. `ort` + DirectML: UVR isolation at ≥15x realtime; WeSpeaker embeddings match Python's within ~1e-3 cosine.
4. WinRT OCR from a dedicated thread; template matching scores match OpenCV's.
5. Tauri 2 window loading the existing UI with a stub bridge.
Report results to the user before Phase 1.

**Phase 1: core pipeline as a CLI** (`cli.py` equivalent): fetch info, download, transcribe, translate, glossary, SRT/transcript, burn.
Port the 66 tests. Output must match the Python version's files closely.

**Phase 2: Tauri app**: bridge, queue, clip tab, editor, history, glossary, settings, setup screen, logs/report.

**Phase 3: live mode**: stream audio, VAD chunking, captions, overlay window, OBS server, chat.

**Phase 4: packaging and updates**: Inno installer (same AppId), first-run component downloads, updater compatibility test from an
installed 2.3.1 → 3.0.0 on a test path before any public release.

## Working rules for this repo

- The Python app keeps shipping until 3.0.0 is ready. Put the Rust code in `rust/` (a Cargo workspace) in this repo; don't move or delete
  the Python code. Work on `main` without switching branches unless the user says otherwise; commit or push only when the user asks.
- This repo has a graphify knowledge graph (`graphify-out/`, git-ignored): run `graphify query "<question>"` before grepping the Python
  source, and `graphify update .` after changing code.
- The user is not deep in the code. Explain progress in plain language, lead with outcomes, and say clearly what wasn't tested.
- Never use an AI service for translation (user requirement): everything runs locally on their GPU.
