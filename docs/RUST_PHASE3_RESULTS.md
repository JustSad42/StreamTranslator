# Rust rewrite: Phase 3 results (2026-11-12)

Phase 3 = live mode: stream audio to captions, Twitch chat translation, the floating caption window and the OBS browser
source. With this, every tab of the app runs on the Rust backend. Code: `rust/crates/st-live` (session, chat, OBS server),
`rust/apps/st-app/src/live.rs` (backend methods, caption window), small additions to Whisper (no built-in VAD, no
timestamps), Silero VAD (minimum speech length) and voice isolation (16 kHz pieces), chat text filters in
`st-core/src/chat.rs`.

## Python vs Rust on real live streams

Same PC (RTX 4080), same settings (large-v3, two-step, glossary on, auto-detect language), French League/variety streams.

| Run | Python | Rust |
|---|---|---|
| zerator, each alone on the GPU, 2 min (one after the other) | 17 captions, delay avg 2.6 s (median 1.5) | 17 captions, delay avg 2.3 s (median 2.1) |
| otplol_, both at once on one GPU, 3 min | 14 captions, delay avg 9.5 s | 14 captions, delay avg 10.6 s |
| Time to "Listening…" | 5.2–6.6 s | 4.4–5.6 s |
| Language detection | French, locked after 2 lines | French, locked after 2 lines |

- Running both at once on the same audio, both versions cut the stream at the same moments, and the first three captions
  were word-for-word identical. Later lines differ because each cut depends on when audio arrived and when the
  previous line finished. "Delay" is how far the caption is behind the stream (wait for the pause + translation time).
- Saved files match Python's format exactly: `Live\<channel>_<date>.transcript.txt` (with the original line under each
  English line), `.en.srt`, and the `.jpg` preview saved at the end (Windows line endings, same layout).
- Rust delay alone on the GPU was 2.0–3.9 s across all runs.

## Chat

- Same Twitch chat at the same time (zerator, French): Python translated 28 of 30 messages, Rust 21 of 23 in its shorter
  window. Same messages got the same English (e.g. "vive HC ! ^^" → "Hail to H.C.! What?").
- Chat filters (laughs like "jajaja"/"kkkk", English detection, emotes, @mentions) were checked against Python's own code on
  3,000 generated messages: 3,000/3,000 identical. The 10 remaining Python test cases (chat filters) are ported, so **all 66
  Python test cases now pass in Rust**.
- Note: a chat that's quiet, follower-only or subscriber-only shows nothing in either version (otplol_ was sub-only, gaules
  quiet). Not a bug.

## End-to-end UI test (`rust/ui-tests/live.mjs`)

Drives the real app's Live tab on a live channel. **10/10 pass** (zerator, 45–75 s runs):
Go live → captions + drafts + language lock → stream preview and level meter → floating caption window shows the captions →
OBS browser source (`http://127.0.0.1:47851/`) streams them → chat connects and translates → change a setting while live →
Stop: transcript, SRT and History entry with thumbnail → chat stops too → an unknown channel shows a friendly error.

The caption window was also checked on the real desktop: a see-through dark box at the bottom centre, always on top of other
apps, text 36 px at 120% interface size, original line underneath.

Bugs found and fixed by these tests:
- The caption window didn't open: WebView2 requires every window of an app to start with the same browser flags.
- Captions never reached it: Tauri event names can't contain dots.

## Not tested yet

- Voice isolation during live mode (the code path is ported and builds; not run on a stream).
- YouTube and Kick live links (only Twitch channels were tested).
- Hours-long sessions (longest run was 3 minutes), and a stream ending while live.
- Dragging, scroll-to-resize and right-click-to-hide on the caption window by hand (the code is there; tests can't move the mouse
  over it). Python's version can't be compared on the same screen at the same time.
- Live mode on a PC without NVIDIA (CPU).

## Found along the way

- Live audio sometimes stops when the stream's audio format changes (likely Twitch ad insertion). ffmpeg then exits with
  "Invalid argument". Python uses the same ffmpeg command, so it can happen there too. Both versions reconnect (Rust: within
  2.5 s, seen twice in one 2-minute run). Rust now writes ffmpeg's last error line to `app.log` when this happens. A possible
  later fix is to make ffmpeg tolerate the change instead of reconnecting.
- Whisper sometimes invents a line during noise or music (e.g. "I would like to ask the Commissioner…" on a stream with no
  speech). Same model and settings as Python, so Python does this too.
