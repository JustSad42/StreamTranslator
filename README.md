# StreamTranslator

Translate other streamers into English with Whisper on your own GPU. Nothing is sent to an AI service.

Open **StreamTranslator** from the Start menu. (From the source folder: double-click `StreamTranslator.pyw`.)

## New in 2.1

- **Two-step translation (default):** Whisper writes down what was said in the original language, then
  Meta's NLLB translator (on your GPU) turns it into English. It keeps every sentence, which Whisper's
  built-in translation tends to drop. Choose **Turbo** for ~3x faster jobs, or **Whisper only** for the old method.
  Translator size (Better 1.3B / Standard 600M) is in Settings and also applies to chat.
- **Reads who's in the game:** champion portraits (matched against Riot's icons) and player names
  (OCR) are fed to Whisper for that job. Tested: 16 of 20 champions right on two broadcast streams.
- **Subtitles-only jobs download just the audio** (a minute of audio in ~3 s).
- **Small installer (362 MB, was 1.8 GB):** a setup screen on first run finds GPU libraries already on
  the PC (CUDA Toolkit, Faster-Whisper-XXL, Subtitle Edit) or downloads NVIDIA's official ones
  (1.25 GB, checksum-checked), downloads models, or uses a faster-whisper model folder you already have.
  FFmpeg is bundled. Voice isolation now runs on DirectML (any GPU, and faster).
- **Tests:** `.venv\Scripts\python -m pytest -q` (49 tests).

## Clip tab: a VOD section, translated

1. Choose how to find the moment:
   - **Their VOD**: paste their VOD link, or just type their channel name to pick from their recent VODs.
     A link with `?t=1h02m30s` starts the section there.
   - **Sync from my VOD**: give your VOD, when the game started and ended in *your* VOD, and one or more of
     their channels. The app finds each one's VOD covering that moment and maps the times, using when each
     stream went live. It pads ±15 s because streamers' broadcast delays differ (tested: 8 s apart).
     With several channels, **Translate all** queues every streamer from that game.
2. Adjust the section:
   - **Snap to game** reads the in-game clock (Windows' built-in OCR) and sets the section to exactly the
     game: start at 0:00 on the clock, end when the clock disappears. Broadcast replays are skipped.
   - Drag the handles or the purple area, or type times.
   - Scroll the timeline to zoom; hovering shows instant previews.
3. Options: **Isolate voices** removes game sound and music before Whisper listens (GPU, ~18x realtime).
   It's worth turning on for loud teamfights and music-heavy streams.
4. **Translate this section** (Ctrl+Enter). Jobs queue up, so you can keep adding more. You get:
   - `*.subbed.mp4` with English subtitles burned in
   - `*.en.srt`
   - `*.transcript.txt` with VOD timestamps
   - optionally original-language subtitles
5. **Edit subtitles**: watch the clip with the subtitles exactly as they'll be burned in, fix text or timing
   (split, set start/end at the playhead, add or delete lines), then **Save & burn video** to re-burn without
   translating again. The original `.srt` is kept as a backup.

## Live tab: any live stream, translated in real time

Type a channel (or paste any Twitch / YouTube / Kick live link) and press **Go live**. You get:

- Draft captions while they talk, then the final translation about 1–2 s after each sentence.
- **Translate chat**: their Twitch chat, translated on your GPU with Meta's NLLB model (one-time 1.2 GB
  download). Emotes, laughs ("jajaja", "kkkk") and messages already in English are left as they are.
- **Isolate voices**: same as for clips, adds ~0.4 s per line.
- **Floating caption window**: always on top. Drag it over any player. Scroll to change text size.
- **OBS browser source**: click *Get link* and add it in OBS as a Browser source to put the captions on your
  own stream. Add `?size=56&bg=0.4&bottom=8` to the link to change the style.
- A transcript and SRT saved to `Videos\StreamTranslator\Live\`.

Turn off the League glossary for non-League streams.

## History tab

Every clip and live session, with thumbnails. Search finds titles, channels **and what was said** (with
timestamps). From a card you can play, edit subtitles, open the transcript, re-open the section in the Clip tab,
or remove it from the list (files stay on disk).

## Glossary tab: League vocabulary, no AI service

- Before translating, Whisper gets a short list of game words: champions in the game (add them on the Clip or
  Live tab), your own words, then core terms (Baron, Rift Herald, Void Grubs, Flash…).
- Afterwards, leftover foreign terms (*Heraldo → Herald*) and garbled names (*Nafiri → Naafiri*,
  *Casante → K'Sante*) are fixed.
- The champion list comes from Riot's Data Dragon and updates itself. It also works offline.
- Add pro players and nicknames under **Your words**, and permanent corrections under **Fixes**. Test lines
  in **Try it**.
- Stored in `%APPDATA%\StreamTranslator\glossary.json`.

## Settings

- **Interface size**: Auto picks a size from your screen (180% on a 4K monitor at 100% Windows scaling).
  Ctrl + / Ctrl − to adjust, Ctrl 0 to reset.
- **Windows notifications** when a translation or the whole queue finishes while you're in another window.
- **Keep the downloader up to date**: checks for a new yt-dlp at startup (Twitch changes break old versions).
  New versions are checksum-verified and used from the next launch.
- Your Twitch channel (for **My VODs** in sync mode), output folder, and preloading Whisper at startup.
- Shortcuts: Ctrl+1–5 switch tabs. Drag a link onto the window to load it.

## Command line

```
.venv\Scripts\python -m streamtranslator.cli https://www.twitch.tv/videos/123 --start 1:02:30 --end 1:35:00
```

## Limits

- Whisper translates only **into English**, and literally. Slang and caster shorthand can come out odd.
- Chat translation uses a small model: short slangy messages are hit-and-miss.
- Snap to game needs the League HUD or broadcast clock visible at the top of the screen.
- Sub-only VODs can't be downloaded (no login support).
- Live captions trail the stream by a few seconds: the length of each sentence plus about 1 s of processing.

## Building

```
powershell -ExecutionPolicy Bypass -File build.ps1
```

Makes `dist\StreamTranslator\StreamTranslator.exe` and `dist\StreamTranslator-Setup-<version>.exe`
(per-user install, Start menu entry, no admin). Needs the `.venv` set up:

```
uv venv --python 3.12 .venv
uv pip install --python .venv\Scripts\python.exe -r requirements.txt
uv pip uninstall --python .venv\Scripts\python.exe onnxruntime
uv pip install --python .venv\Scripts\python.exe --reinstall onnxruntime-gpu==1.22.0
```

(faster-whisper pulls in the CPU-only onnxruntime, which hides the GPU one, so it's swapped out.)

Needs `ffmpeg` on PATH and an NVIDIA GPU (it falls back to the CPU, which is slow). The app's code is in
`streamtranslator/`, with the interface in `streamtranslator/ui/`.
