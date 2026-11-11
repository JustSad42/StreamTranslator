"""Live mode: translate any live stream in near real time.

ffmpeg pulls the stream's audio. Voice detection cuts it at natural pauses (or
every ~9 s during non-stop talking). Each piece is translated by Whisper and
sent out as a caption. While someone is mid-sentence, a fast draft ("partial")
is shown and then replaced by the final line.
"""
from __future__ import annotations

import base64
import subprocess
import threading
import time
from datetime import datetime
from pathlib import Path
from typing import Callable

import numpy as np

from .pipeline import (NO_WINDOW, JobError, _QuietLogger, _is_junk, ffmpeg_exe, lang_name, load_model,
                       model_device)
from .sync import classify
from .timefmt import format_ts, srt_ts

SR = 16000
MAX_CHUNK_S = 9.0          # force a cut during non-stop talking
PAUSE_S = 0.45             # silence that ends a sentence
PARTIAL_EVERY_S = 1.0


def resolve_live(text: str) -> dict:
    import yt_dlp
    kind, value = classify(text)
    if kind == "vod":
        raise JobError("That's a VOD link. Use the Clip tab for VODs. Live mode needs a channel that's live now.")
    url = f"https://www.twitch.tv/{value}" if kind == "channel" else value
    opts = {"quiet": True, "no_warnings": True, "skip_download": True, "logger": _QuietLogger()}
    try:
        with yt_dlp.YoutubeDL(opts) as y:
            info = y.extract_info(url, download=False)
    except Exception as e:
        msg = str(e).replace("ERROR: ", "")
        if "not currently live" in msg or "offline" in msg.lower():
            raise JobError(f"{value} isn't live right now.") from None
        raise JobError(f"Couldn't open that stream.\n{msg}") from None
    if info.get("_type") == "playlist":
        raise JobError("That link is a playlist, not a live stream.")
    if not info.get("is_live"):
        raise JobError("That stream isn't live right now.")
    fmts = [f for f in info.get("formats") or [] if f.get("url")]
    audio = [f for f in fmts if f.get("vcodec") in (None, "none") and f.get("acodec") not in (None, "none")
             and f.get("format_note") != "storyboard"]
    video = sorted([f for f in fmts if f.get("vcodec") not in (None, "none") and f.get("height")],
                   key=lambda f: f["height"])
    if not audio and not video:
        raise JobError("No playable formats found for that stream.")
    audio_url = (audio or video)[0]["url"]
    preview = next((f for f in video if f["height"] >= 160), video[0] if video else None)
    ua = ((video or audio)[0].get("http_headers") or {}).get("User-Agent", "")
    return {
        "url": url,
        "audio": audio_url,
        "preview": preview["url"] if preview else None,
        "ua": ua,
        "title": info.get("description") or info.get("title") or "",
        "channel": info.get("uploader") or info.get("uploader_id") or info.get("channel") or value,
        "thumbnail": info.get("thumbnail") or "",
    }


class LiveSession:
    def __init__(self, source: str, model: str, language: str | None, show_original: bool,
                 use_glossary: bool, champions: list[str], save_dir: Path,
                 emit: Callable[..., None], isolate: bool = False, method: str = "two-step",
                 translator: str = "better"):
        self.isolate = isolate
        self.method = method
        self.translator = translator
        self.last_frame: bytes | None = None
        self.thumb_path: Path | None = None
        self.source = source
        self.model_name = model
        self.language = language
        self.show_original = show_original
        self.use_glossary = use_glossary
        self.champions = champions
        self.save_dir = Path(save_dir)
        self.emit = emit
        self.stop_event = threading.Event()
        self._lock = threading.Lock()
        self._pending: list[np.ndarray] = []
        self._levels: list[float] = []
        self._received = 0             # samples received since start
        self._proc: subprocess.Popen | None = None
        self._votes: dict[str, int] = {}
        self.locked_lang: str | None = language
        self.lines = 0
        self.started = time.time()
        self.info: dict = {}
        self.transcript_path: Path | None = None
        self._srt_path: Path | None = None
        self._threads: list[threading.Thread] = []

    # ---- lifecycle
    def start(self):
        t = threading.Thread(target=self._main, daemon=True)
        t.start()
        self._threads.append(t)

    def stop(self):
        self.stop_event.set()
        if self._proc and self._proc.poll() is None:
            self._proc.kill()

    def update(self, language=..., show_original=None, champions=None, use_glossary=None, isolate=None,
               method=None):
        if isolate is not None:
            self.isolate = isolate
        if method is not None:
            self.method = method
        if language is not ...:
            self.language = language
            self.locked_lang = language
            self._votes.clear()
        if show_original is not None:
            self.show_original = show_original
        if champions is not None:
            self.champions = champions
        if use_glossary is not None:
            self.use_glossary = use_glossary

    def _main(self):
        try:
            self.emit("live.status", state="connecting", message="Finding the stream…")
            self.info = resolve_live(self.source)
            self.emit("live.status", state="loading", message=f"Loading Whisper {self.model_name}…",
                      channel=self.info["channel"], title=self.info["title"], thumbnail=self.info["thumbnail"],
                      url=self.info["url"])
            self.model = load_model(self.model_name)
            self._open_files()
            for target in (self._reader, self._processor, self._thumbs, self._meter):
                t = threading.Thread(target=target, daemon=True)
                t.start()
                self._threads.append(t)
            self.emit("live.status", state="live", message="Listening…", device=model_device(self.model_name),
                      channel=self.info["channel"], title=self.info["title"], url=self.info["url"],
                      transcript=str(self.transcript_path))
            self.stop_event.wait()
        except JobError as e:
            self.emit("live.error", message=str(e))
        except Exception as e:  # keep the app alive whatever happens
            self.emit("live.error", message=f"{type(e).__name__}: {e}")
        finally:
            self.stop_event.set()
            if self._proc and self._proc.poll() is None:
                self._proc.kill()
            if self.last_frame and self.transcript_path:
                try:
                    self.thumb_path = self.transcript_path.with_suffix("").with_suffix(".jpg")
                    self.thumb_path.write_bytes(self.last_frame)
                except OSError:
                    self.thumb_path = None
            self.emit("live.status", state="stopped", message="Stopped.", lines=self.lines,
                      transcript=str(self.transcript_path) if self.transcript_path else None)

    def _open_files(self):
        folder = self.save_dir / "Live"
        folder.mkdir(parents=True, exist_ok=True)
        stamp = datetime.now().strftime("%Y-%m-%d_%H-%M")
        safe = "".join(c if c.isalnum() or c in "-_" else "_" for c in self.info["channel"])[:30]
        self.transcript_path = folder / f"{safe}_{stamp}.transcript.txt"
        self._srt_path = folder / f"{safe}_{stamp}.en.srt"
        self.transcript_path.write_text(
            f"Live translation of {self.info['channel']}\n{self.info['url']}\n"
            f"Started {datetime.now():%Y-%m-%d %H:%M}\n\n", encoding="utf-8")
        self._srt_path.write_text("", encoding="utf-8")

    # ---- audio in
    def _reader(self):
        failures = 0
        while not self.stop_event.is_set():
            cmd = [ffmpeg_exe(), "-hide_banner", "-nostdin", "-loglevel", "error"]
            if self.info.get("ua"):
                cmd += ["-user_agent", self.info["ua"]]
            cmd += ["-i", self.info["audio"], "-vn", "-ac", "1", "-ar", str(SR), "-f", "f32le", "-"]
            self._proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                          creationflags=NO_WINDOW)
            got_any = False
            while not self.stop_event.is_set():
                data = self._proc.stdout.read(SR // 10 * 4)       # 0.1 s
                if not data:
                    break
                got_any = True
                a = np.frombuffer(data, dtype=np.float32)
                rms = float(np.sqrt(np.mean(a * a))) if a.size else 0.0
                db = 20 * np.log10(max(rms, 1e-6))
                with self._lock:
                    self._pending.append(a)
                    self._received += len(a)
                    self._levels.append(round(max(0.0, min(1.0, (db + 60) / 50)), 3))
            if self._proc.poll() is None:
                self._proc.kill()
            if self.stop_event.is_set():
                break
            failures = 0 if got_any else failures + 1
            if failures >= 5:
                self.emit("live.error", message="Lost the stream and couldn't reconnect. It may have ended.")
                self.stop_event.set()
                break
            self.emit("live.status", state="reconnecting", message="Stream dropped. Reconnecting…")
            time.sleep(min(2 * failures + 1, 8))
            try:
                fresh = resolve_live(self.source)
                self.info.update(audio=fresh["audio"], preview=fresh["preview"])
                self.emit("live.status", state="live", message="Listening…")
            except JobError as e:
                if "isn't live" in str(e):
                    self.emit("live.error", message="The stream ended.")
                    self.stop_event.set()

    def _meter(self):
        """Twitch delivers audio in ~2 s bursts. Replay the levels at real-time pace
        so the meter moves smoothly instead of jumping once per burst."""
        while not self.stop_event.wait(0.1):
            with self._lock:
                if len(self._levels) > 40:          # fell behind: skip ahead
                    del self._levels[:-20]
                lv = self._levels.pop(0) if self._levels else None
            if lv is not None:
                self.emit("live.level", level=lv)

    def _thumbs(self):
        while not self.stop_event.wait(1 if not hasattr(self, "_thumb_once") else 20):
            self._thumb_once = True
            url = self.info.get("preview")
            if not url:
                return
            cmd = [ffmpeg_exe(), "-hide_banner", "-nostdin", "-loglevel", "error", "-i", url,
                   "-frames:v", "1", "-vf", "scale=640:-2", "-f", "image2pipe", "-vcodec", "mjpeg", "-"]
            try:
                r = subprocess.run(cmd, capture_output=True, timeout=25, creationflags=NO_WINDOW)
                if r.returncode == 0 and r.stdout:
                    self.last_frame = r.stdout
                    self.emit("live.frame", data="data:image/jpeg;base64," + base64.b64encode(r.stdout).decode())
            except subprocess.TimeoutExpired:
                pass

    # ---- speech out
    def _processor(self):
        from faster_whisper.vad import VadOptions, get_speech_timestamps
        vad_opts = VadOptions(min_silence_duration_ms=350, speech_pad_ms=150, min_speech_duration_ms=200)
        buf = np.zeros(0, dtype=np.float32)
        buf_start = 0                      # absolute sample index of buf[0]
        last_partial = 0.0
        pad = int(0.15 * SR)
        while not self.stop_event.is_set():
            time.sleep(0.25)
            with self._lock:
                if self._pending:
                    buf = np.concatenate([buf, *self._pending])
                    self._pending.clear()
            if len(buf) < 0.8 * SR:
                continue
            speech = get_speech_timestamps(buf, vad_opts)
            if not speech:
                keep = int(0.4 * SR)
                buf_start += max(0, len(buf) - keep)
                buf = buf[-keep:]
                continue
            first = max(0, speech[0]["start"] - pad)
            if first:
                buf, buf_start = buf[first:], buf_start + first
                speech = [{"start": s["start"] - first, "end": s["end"] - first} for s in speech]
            trailing = len(buf) - speech[-1]["end"]
            if trailing >= PAUSE_S * SR or len(buf) >= MAX_CHUNK_S * SR:
                if trailing >= PAUSE_S * SR:
                    cut = min(len(buf), speech[-1]["end"] + pad)
                else:
                    gaps = [(speech[i + 1]["start"] - speech[i]["end"], speech[i]["end"])
                            for i in range(len(speech) - 1) if speech[i]["end"] > 3 * SR]
                    cut = max(gaps)[1] + pad // 2 if gaps and max(gaps)[0] > 0.12 * SR else len(buf)
                chunk = buf[:cut]
                try:
                    self._final(chunk, buf_start, buf_start + cut)
                except Exception as e:
                    self.emit("live.error", message=f"Translation error: {e}", fatal=False)
                buf, buf_start = buf[cut:], buf_start + cut
                last_partial = time.time()
            elif time.time() - last_partial >= PARTIAL_EVERY_S and speech[-1]["end"] - speech[0]["start"] > 1.2 * SR:
                last_partial = time.time()
                try:
                    self._partial(buf)
                except Exception:
                    pass

    def _hot(self) -> str | None:
        if not self.use_glossary:
            return None
        from .glossary import Glossary
        return Glossary.get().hotwords(self.champions)

    def _fix(self, text: str, english: bool) -> str:
        if not self.use_glossary:
            return text
        from .glossary import Glossary
        return Glossary.get().fix(text, english=english)[0]

    def _detect(self, chunk: np.ndarray) -> str:
        if self.locked_lang:
            return self.locked_lang
        lang, prob, _ = self.model.detect_language(audio=chunk)
        if prob >= 0.5:
            self._votes[lang] = self._votes.get(lang, 0) + (2 if prob >= 0.9 else 1)
        best = max(self._votes.items(), key=lambda kv: kv[1], default=(lang, 0))
        if best[1] >= 3:
            self.locked_lang = best[0]
            self.emit("live.lang", lang=best[0], name=lang_name(best[0]), locked=True)
        else:
            self.emit("live.lang", lang=lang, name=lang_name(lang), locked=False, prob=round(prob, 2))
        return lang

    def _clean(self, chunk):
        """Optional voice isolation (game sound / music removed) before Whisper."""
        if not self.isolate:
            return chunk
        try:
            from . import isolate
            if not isolate.model_ready():
                self.emit("live.status", state="live", message="Downloading the voice isolation model (66 MB)…")
                isolate.download_model()
                self.emit("live.status", state="live", message="Listening…")
            return isolate.voice_16k_from_16k(chunk)
        except Exception as e:
            self.isolate = False
            self.emit("live.error", message=f"Voice isolation turned off: {e}", fatal=False)
            return chunk

    def _transcribe(self, chunk, lang, task, beam, hot):
        segs, _ = self.model.transcribe(chunk, language=lang, task=task, beam_size=beam, vad_filter=False,
                                        condition_on_previous_text=False, without_timestamps=True,
                                        hotwords=hot, no_speech_threshold=0.6)
        from .glossary import Glossary
        gl = Glossary.get()
        parts = []
        for s in segs:
            t = " ".join(s.text.split())
            if t and not _is_junk(s, t) and not gl.is_prompt_echo(t, hot):
                parts.append(t)
        return " ".join(parts)

    def _two_step(self, lang: str) -> bool:
        from . import translate
        return self.method == "two-step" and lang != "en" and translate.supported(lang)

    def _to_english(self, text: str, lang: str, beam: int) -> str:
        from .translate import Translator
        tr = Translator.get(self.translator, lambda s: self.emit("live.status", state="live", message=s))
        return tr.translate([text], lang, beam=beam)[0]

    def _speech(self, chunk, lang: str, beam: int) -> tuple[str, str]:
        """(english, original) for a chunk, with either method."""
        if self._two_step(lang):
            from .glossary import Glossary
            hot = Glossary.get().hotwords(self.champions, core=False) if self.use_glossary else None
            orig = self._transcribe(chunk, lang, "transcribe", beam, hot)
            if not orig:
                return "", ""
            return self._fix(self._to_english(orig, lang, 2 if beam == 1 else 4), True), self._fix(orig, False)
        task = "transcribe" if lang == "en" else "translate"
        text = self._transcribe(chunk, lang, task, beam, self._hot())
        if not text:
            return "", ""
        orig = ""
        if beam > 1 and self.show_original and lang != "en":
            orig = self._fix(self._transcribe(chunk, lang, "transcribe", 3, None), False)
        return self._fix(text, True), orig

    def _partial(self, buf):
        lang = self.locked_lang
        if not lang:
            return
        text, _ = self._speech(self._clean(buf), lang, 1)
        if text and not self.stop_event.is_set():
            self.emit("live.partial", text=text)

    def _final(self, chunk, start_sample, end_sample):
        t_proc = time.time()
        chunk = self._clean(chunk)
        lang = self._detect(chunk)
        text, orig = self._speech(chunk, lang, 5)
        if not text:
            self.emit("live.partial", text="")
            return
        if not self.show_original:
            orig = ""
        with self._lock:
            received = self._received
        delay = (received - end_sample) / SR
        t0, t1 = start_sample / SR, end_sample / SR
        self.lines += 1
        self.emit("live.caption", id=self.lines, text=text, orig=orig, lang=lang, t=round(t0, 1),
                  clock=datetime.now().strftime("%H:%M:%S"), delay=round(delay, 1),
                  took=round(time.time() - t_proc, 2))
        try:
            with open(self.transcript_path, "a", encoding="utf-8") as f:
                f.write(f"[{datetime.now():%H:%M:%S} | +{format_ts(t0)}]  {text}\n")
                if orig:
                    f.write(f"{'':>22}({orig})\n")
            with open(self._srt_path, "a", encoding="utf-8") as f:
                f.write(f"{self.lines}\n{srt_ts(t0)} --> {srt_ts(max(t1, t0 + 1.2))}\n{text}\n\n")
        except OSError:
            pass
