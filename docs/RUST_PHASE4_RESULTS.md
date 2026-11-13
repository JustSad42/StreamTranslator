# Rust rewrite: Phase 4 results (2026-11-12)

Phase 4 = packaging and updates: a 3.0.0 installer that the installed Python 2.x apps update to in place, first-run
downloads, notifications, uninstall. Nothing was published; the installer exists only in `rust/dist`.

New files: `rust/installer/StreamTranslator.iss`, `rust/build-installer.ps1`, `rust/ui-tests/firstrun.mjs`; the app gained
`StreamTranslator.exe --selftest <file>`.

## The installer

| | Python 2.3.1 | Rust 3.0.0 |
|---|---|---|
| Installer | 362 MB | 100 MB |
| Installed | 1.1 GB | 378 MB (exe 88 MB, ffmpeg, yt-dlp, ONNX Runtime + DirectML, VC++ runtime) |
| GPUs | CTranslate2 wheel (Python) | compiled for GTX 10 to RTX 50 (6.1–12.0, plus PTX for newer) |

- Same AppId, install folder (`%LOCALAPPDATA%\Programs\StreamTranslator`), `AppMutex`, file name pattern
  (`StreamTranslator-Setup-<version>.exe`), per-user install without admin, and the `[Run] Check: WizardSilent` entry that reopens
  the app after a silent update. It removes the Python version's `_internal` folder.
- GPU libraries and models still download on first run, so the installer doesn't carry them.
- WebView2: the installer checks for it and, if missing, fetches Microsoft's small online installer and runs it. Windows 11 and
  current Windows 10 already have it. **Not tested on a PC without WebView2.**
- GTX 900-series cards are not supported: CTranslate2 4.8 has GPU code that needs a GTX 10 or newer. (Python 2.3.1 used
  CTranslate2 4.9 from PyPI; whether that ran on GTX 900 wasn't checked.)
- The build script pins and checksum-verifies ONNX Runtime 1.24.4, DirectML 1.15.4, yt-dlp 2026.10.26 and Silero VAD v6,
  bundles the same ffmpeg as the Python build, runs the app's self-test from the collected folder, then builds the installer.
- The release build of the app passed the full clip/tab UI test (19/19) before the installer was used.

## Update test on the real installed app (with the user's OK)

1. **2.3.1 → 3.0.0**, using 2.3.1's own update code (`appupdate.py`, identical to the v2.3.1 tag), with the installer served from
   this PC instead of GitHub: download + SHA-256 check, notes saved, `updated_from` recorded, background PowerShell helper,
   silent Inno install. **Worked**: installed in ~30 s, `_internal` removed, 3.0.0 reopened by itself, Add/Remove Programs shows
   3.0.0, Start menu shortcut updated.
2. **"What's new in 3.0.0"** appeared, "Updated from 2.3.1", with the saved notes. (On the first try it didn't, because an earlier
   test install had already marked 3.0.0 as seen in the real settings. A test artifact, not a bug.)
3. **"Go back to the old version"** in the Rust app: downloaded the real 2.3.1 installer from GitHub, verified it, closed, installed
   it silently, and 2.3.1 reopened by itself and later closed normally. This also covers the Rust app's own updater end to end
   (same code as updating to a newer version).
4. Cleanup: the user's install is back to 2.3.1 only; settings restored from a backup made before the test; history unchanged.

Found: going back to 2.3.1 leaves the Rust files (ffmpeg, yt-dlp, DLLs) next to the Python app, because the Python installer
doesn't know about them. 2.3.1 ran fine with them there; they were removed by hand after the test. Worth handling if "Go back"
to 2.x stays available after 3.0.0 ships (for example, the Rust app deleting them before handing over to an older installer).

## Separate test install ("StreamTranslator Test", own AppId and folder)

- Silent install, app reopened, Start menu entry registered with the app's ID; a Windows notification under that ID shows
  "StreamTranslator Test" with the app's icon. So notifications from the installed app carry its name, not "PowerShell".
- Uninstall while the app is running: the uninstaller refuses (in silent mode it cancels), same as the Python version. After closing
  the app: files, shortcut and registry entry removed; settings and downloaded components kept.

## First run on a "new PC" (empty settings, data and model folders)

Setup screen appears; GPU libraries (cuBLAS + cuDNN, 1.3 GB from PyPI, checksum-verified) downloaded in 25 s; Whisper small in
13 s; a model then loaded on the GPU with the downloaded libraries. The interface also preloaded the default Whisper model
(large-v3) once the GPU libraries were ready, as the Python app does.

## Not tested

- A PC without WebView2, without an NVIDIA GPU (CPU fallback), or with an older/newer NVIDIA generation than RTX 40.
- Code signing: neither version is signed, so Windows SmartScreen warns on first download, as it does today.
- Publishing: `release.ps1` still builds the Python app. Switching releases to the Rust build is a separate, deliberate step.

## Before releasing 3.0.0

- Decide what release.ps1 should build (Python or Rust) and write real release notes.
- Optionally handle the leftover files on "Go back" to 2.x.
- Try it on a second PC if possible (different GPU, Windows 10).
