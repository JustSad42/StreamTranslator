# Team voice: feasibility tests (2026-11-16)

Goal: live English captions on OBS for an Egyptian League team that speaks Egyptian Arabic mixed with English and
League terms, several speakers at once, from a Discord bot or one audio device. These tests decide the approach
before any app code is written.

## 1. Egyptian Arabic comms → English

Test audio: the first 5 min 20 s of Anubis Gaming's public "Arabian League Voice Comms 5th Game Vs Fox Gaming"
(YouTube TxhgCWhQi6w): five players, Egyptian Arabic with English sentences and League terms mixed in, game sound
underneath. Every model got the same audio with the same settings (language Arabic, voice detection on, beam 5),
on the RTX 4080.

| Model | Whisper translates to English | Whisper writes Arabic → NLLB translates | Time (5:20 of audio) |
|---|---|---|---|
| **large-v3** (what the app uses now) | **Best.** Short, accurate comms in English | Rewrites the dialect as formal Arabic and loses meaning ("I go pray" became "I'm going to shoot you") | 29 s translate, 106 s transcribe |
| large-v3-turbo | Unusable: outputs Arabic, not English (turbo wasn't trained to translate) | Formal Arabic, meaning drifts | 36 s |
| Egyptian code-switching large-v2 (Mano200600) | Close to large-v3, a bit choppier ("Neko no R", "I got the money") | Keeps English words and the dialect as spoken; NLLB still misreads dialect ("the admin is telling me no" → "Addicted to saying no") | 32 s / 115 s |
| Egyptian code-switching medium (Seif-Eldeen-Sameh) | Good at the start, then repeats itself in loops | Falls into repetition loops ("لأ، لأ، لأ…" for 30 s) | 55 s / 101 s |

Example, the same moment (2:11–2:21) with large-v3 translating directly:

> Don't flash, Serfine, don't flash. · Good job. · Recall base, base, base, everyone. · We have no ulti, Kled has no ulti. ·
> Kha'Zix is back, Kha'Zix is back. · I'm coming, coming from behind. · TP top, TP top. · Niko no R, Niko no R.

**Result:** use **large-v3 with Whisper's own translation** for team voice, not the two-step NLLB route the clip mode
defaults to. Egyptian dialect is where NLLB is weakest, and Whisper's translation keeps the English parts and League
calls intact. Speed is fine for live: 5 min 20 s of comms translated in 29 s (about 11x faster than real time) leaves
room for several speakers on one GPU. The code-switching models were not better; they stay as an option through the
app's existing custom model setting, not the default.

Not tested yet: your team's own voices and Discord audio quality (Opus at 64 kbps, per-person, no game sound mixed in),
which should be easier than this recording.

## 2. Discord bot hearing each person (end-to-end encrypted calls)

Since March 2026 every Discord call is end-to-end encrypted (DAVE) and bots must support it to hear anything.
songbird 0.6 (Rust, April 2026) added DAVE support, including receiving voice.

A test bot is built (`discord-voice-spike.exe`, songbird 0.6.1 + serenity 0.12.5): it joins a voice channel, records
each person as a separate WAV named after them for a set time, and reports how much clean audio arrived per person.
It needs no privileged intents (only Guilds and Voice States) and no message permissions.

**Result (2026-11-16, a real team call, 60 s): it works.** The bot joined the encrypted call and recorded 5 people
as separate streams named after their Discord accounts (3 talking, 2 quiet). The audio is clean speech, and
Whisper large-v3 translated the Egyptian Arabic well. Names Whisper had never heard came out mangled ("BrokeVein"),
which is why Discord mode now gives Whisper everyone's name in the call as a hint.

## 3. Discord mode in the app (the recorded call replayed as five people)

- Everyone listed by their Discord name; every line named after the right person (no voice learning needed).
- Delay after each sentence: about 0.5 s on average, 1.0 s at worst, with three people talking at once.
- Before capping Whisper's retries, one unclear piece took 9.7 s and held up everyone's captions; now at most one
  retry and a length cap, so a loop can't run long. Repeated phrases ("X, but X") are trimmed to one.
- Renaming someone updates the caption window and OBS at once; it's saved, and can be undone.

## Decision so far

- Speech: large-v3, Whisper translation, per speaker, glossary on top.
- Audio-device mode: no blockers.
- Discord mode: works, and is the better choice when the bot can join the server (each person separate, real names).
