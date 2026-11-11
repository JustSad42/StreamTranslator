"""Download a VOD section -> detect language -> Whisper translate -> SRT / transcript -> burned-in MP4."""
from __future__ import annotations

import re
import shutil
import subprocess
import sys
import textwrap
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable
from urllib.parse import parse_qs, urlparse

from .timefmt import format_hms_tag, format_ts, parse_ts, srt_ts

NO_WINDOW = subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0

LANGUAGES = {
    "ar": "Arabic", "bg": "Bulgarian", "cs": "Czech", "da": "Danish", "de": "German",
    "el": "Greek", "en": "English", "es": "Spanish", "fi": "Finnish", "fr": "French",
    "he": "Hebrew", "hi": "Hindi", "hr": "Croatian", "hu": "Hungarian", "id": "Indonesian",
    "it": "Italian", "ja": "Japanese", "ko": "Korean", "lt": "Lithuanian", "ms": "Malay",
    "nl": "Dutch", "no": "Norwegian", "pl": "Polish", "pt": "Portuguese", "ro": "Romanian",
    "ru": "Russian", "sk": "Slovak", "sr": "Serbian", "sv": "Swedish", "th": "Thai",
    "tl": "Tagalog", "tr": "Turkish", "uk": "Ukrainian", "vi": "Vietnamese", "zh": "Chinese",
}

# Whisper's usual hallucinations on music / game noise / silence.
_JUNK = [
    "thank you for watching", "thanks for watching", "please subscribe", "like and subscribe",
    "subtitles by", "amara.org", "subtitled by", "transcribed by", "copyright",
    "시청해주셔서 감사합니다", "구독과 좋아요", "mbc 뉴스", "ご視聴ありがとうございました",
    "字幕", "請不吝點贊", "продолжение следует", "sous-titrage", "untertitel",
]

POSITIONS = {
    # label: (ASS alignment, vertical margin on a 288-line canvas)
    "Above game HUD": (2, 46),
    "Bottom": (2, 12),
    "Top": (8, 14),
}
FONT_SIZES = {"Small": 12, "Medium": 14, "Large": 17}


class Cancelled(Exception):
    pass


class JobError(Exception):
    pass


def lang_name(code: str | None) -> str:
    return LANGUAGES.get(code or "", code or "unknown")


def ffmpeg_exe() -> str:
    if getattr(sys, "frozen", False):                 # the installer ships its own copy
        bundled = Path(getattr(sys, "_MEIPASS", Path(sys.executable).parent)) / "ffmpeg" / "ffmpeg.exe"
        if bundled.exists():
            return str(bundled)
    exe = shutil.which("ffmpeg")
    if not exe:
        raise JobError("ffmpeg was not found. Install it with:  winget install Gyan.FFmpeg")
    return exe


# ---------------------------------------------------------------- VOD info

@dataclass
class VodInfo:
    url: str
    id: str
    title: str
    channel: str
    duration: float
    streams: dict[str, str]          # quality label -> HLS playlist URL
    preview_stream: str
    user_agent: str = ""
    url_start: float | None = None   # ?t= from the pasted link
    start_ts: float | None = None    # unix time the broadcast started (for VOD sync)
    thumbnail: str = ""
    storyboard: dict | None = None   # Twitch sprite sheets for instant hover previews
    site: str = "twitch"             # twitch / youtube / kick / other
    video_only: set = field(default_factory=set)   # quality labels whose stream has no sound (YouTube)

    def to_dict(self) -> dict:
        return {"url": self.url, "id": self.id, "title": self.title, "channel": self.channel,
                "duration": self.duration, "url_start": self.url_start, "start_ts": self.start_ts,
                "thumbnail": self.thumbnail, "storyboard": self.storyboard, "site": self.site,
                "link": vod_link(self, 0)}


class _QuietLogger:
    def debug(self, msg): pass
    def info(self, msg): pass
    def warning(self, msg): pass
    def error(self, msg): pass


def timestamp_from_url(url: str) -> float | None:
    try:
        t = parse_qs(urlparse(url).query).get("t", [None])[0]
        return parse_ts(t) if t else None
    except ValueError:
        return None


_KICK_VOD = re.compile(r"kick\.com/(?:[\w-]+/)?videos?/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})", re.I)


def _kick_info(url: str, opts: dict) -> dict | None:
    """yt-dlp's Kick VOD reader breaks whenever Kick changes its API. Kick's own video API
    still gives the stream playlist, which yt-dlp reads fine."""
    m = _KICK_VOD.search(url)
    if not m:
        return None
    import json
    import urllib.request
    import yt_dlp
    try:
        req = urllib.request.Request(f"https://kick.com/api/v1/video/{m.group(1)}",
                                     headers={"User-Agent": "Mozilla/5.0", "Accept": "application/json"})
        with urllib.request.urlopen(req, timeout=15) as r:
            d = json.loads(r.read().decode("utf-8"))
        with yt_dlp.YoutubeDL(opts) as ydl:
            info = ydl.extract_info(d["source"], download=False)
    except Exception:
        return None
    ls = d.get("livestream") or {}
    start = ls.get("start_time") or d.get("created_at")
    try:
        from datetime import datetime
        start_ts = datetime.fromisoformat(str(start).replace("Z", "+00:00")).timestamp() if start else None
    except ValueError:
        start_ts = None
    info.update(extractor_key="Kick", id=m.group(1), webpage_url=url, title=ls.get("session_title") or "Kick VOD",
                uploader=(ls.get("channel") or {}).get("slug") or "kick", thumbnail=ls.get("thumbnail") or "",
                timestamp=start_ts, duration=info.get("duration") or (ls.get("duration") or 0) / 1000)
    return info


def fetch_info(url: str) -> VodInfo:
    import yt_dlp

    url = url.strip()
    if not url:
        raise JobError("Paste a VOD link first (Twitch, YouTube or Kick).")
    if url.isdigit():
        url = f"https://www.twitch.tv/videos/{url}"
    opts = {"quiet": True, "no_warnings": True, "skip_download": True,
            "logger": _QuietLogger(), "noprogress": True}
    try:
        with yt_dlp.YoutubeDL(opts) as ydl:
            info = ydl.extract_info(url, download=False)
    except Exception as e:  # yt-dlp raises DownloadError with a readable message
        msg = re.sub(r"\x1b\[[0-9;]*m", "", str(e)).replace("ERROR: ", "")
        kick = _kick_info(url, opts)
        if kick:
            info = kick
        else:
            info = None
    if info is None:
        hint = ("\n\nSubscriber-only VODs can't be downloaded without logging in." if "twitch" in url.lower()
                else "\n\nMembers-only, private and age-restricted videos can't be downloaded." if "youtu" in url.lower()
                else "")
        raise JobError(f"Couldn't read that VOD.\n{msg}{hint}") from None

    if info.get("_type") == "playlist":
        raise JobError("That link is a channel or playlist. Paste a single VOD link "
                       "(twitch.tv/videos/123…, a YouTube video, or a Kick video).")
    if info.get("is_live"):
        raise JobError("That stream is live. Use the Live tab, or wait for the VOD.")

    key = (info.get("extractor_key") or "").lower()
    site = "twitch" if "twitch" in key else "youtube" if "youtube" in key else "kick" if "kick" in key else "other"
    has_audio = lambda f: f.get("acodec") not in (None, "none") or (f.get("acodec") is None and site != "youtube")
    vids = [f for f in info.get("formats") or []
            if f.get("url") and f.get("vcodec") not in (None, "none") and f.get("height")]
    if not vids:
        raise JobError("No playable video formats found for that link.")
    # at equal height prefer streams with sound, then H.264 (plays everywhere, copies cleanly into .mp4)
    vids.sort(key=lambda f: (f.get("height") or 0, has_audio(f), str(f.get("vcodec", "")).startswith("avc"),
                             f.get("fps") or 0, f.get("tbr") or 0))

    def best_under(h: int):
        under = [f for f in vids if f["height"] <= h]
        return (under or vids[:1])[-1]

    picks = {"Source": vids[-1], "720p": best_under(720), "480p": best_under(480)}
    streams = {k: f["url"] for k, f in picks.items()}
    auds = [f for f in info.get("formats") or [] if f.get("url") and f.get("vcodec") in (None, "none")
            and f.get("acodec") not in (None, "none") and f.get("format_note") != "storyboard"]
    # AAC first (fits .mp4/.m4a as is), skip YouTube's "DRC" loudness-squashed copies, then best bitrate
    auds.sort(key=lambda f: ("drc" not in str(f.get("format_id", "")).lower(),
                             str(f.get("acodec", "")).startswith("mp4a"), f.get("abr") or f.get("tbr") or 0))
    if auds:
        streams["Audio"] = auds[-1]["url"]
    video_only = {k for k, f in picks.items() if not has_audio(f)}
    if video_only and "Audio" not in streams:
        raise JobError("That video has no downloadable sound track.")
    preview = next((f for f in vids if f["height"] >= 360), vids[-1])["url"]
    ua = (vids[-1].get("http_headers") or {}).get("User-Agent", "")

    storyboard = None
    sbs = [f for f in info.get("formats") or [] if f.get("format_note") == "storyboard" and f.get("fragments")]
    if sbs:
        sb = max(sbs, key=lambda f: f.get("width") or 0)
        if sb.get("fps"):
            storyboard = {"sheets": [fr["url"] for fr in sb["fragments"]], "cols": sb.get("columns") or 5,
                          "rows": sb.get("rows") or 10, "w": sb.get("width"), "h": sb.get("height"),
                          "interval": 1 / sb["fps"]}

    return VodInfo(
        start_ts=float(info["timestamp"]) if info.get("timestamp") else None,
        thumbnail=(info.get("thumbnail") or "").replace("0x0", "640x360"),
        storyboard=storyboard,
        site=site,
        video_only=video_only,
        url=info.get("webpage_url") or url,
        id=str(info.get("id") or "").lstrip("v") if site == "twitch" else str(info.get("id") or ""),
        title=info.get("title") or info.get("description") or "Untitled",
        channel=info.get("uploader") or info.get("uploader_id") or info.get("channel") or "streamer",
        duration=float(info.get("duration") or 0),
        streams=streams,
        preview_stream=preview,
        user_agent=ua,
        url_start=timestamp_from_url(url),
    )


def vod_link(info: VodInfo, seconds: float) -> str:
    """A link that opens the VOD at `seconds`."""
    s = max(0, int(seconds))
    if info.site == "twitch" and info.id:
        return f"https://www.twitch.tv/videos/{info.id}?t={format_hms_tag(s)}"
    if info.site == "youtube" and info.id:
        return f"https://www.youtube.com/watch?v={info.id}&t={s}s"
    return info.url


twitch_link = vod_link


# ---------------------------------------------------------------- ffmpeg helpers

def _ua_args(ua: str) -> list[str]:
    return ["-user_agent", ua] if ua else []


def grab_frame(info: VodInfo, seconds: float, width: int = 480) -> bytes:
    """One JPEG frame from the VOD at `seconds` (for the preview thumbnails)."""
    cmd = [ffmpeg_exe(), "-hide_banner", "-nostdin", "-loglevel", "error",
           *_ua_args(info.user_agent), "-ss", f"{max(0.0, seconds):.2f}", "-i", info.preview_stream,
           "-frames:v", "1", "-vf", f"scale={width}:-2", "-f", "image2pipe", "-vcodec", "mjpeg", "-"]
    r = subprocess.run(cmd, capture_output=True, timeout=30, creationflags=NO_WINDOW)
    if r.returncode != 0 or not r.stdout:
        raise JobError(r.stderr.decode(errors="replace").strip() or "frame grab failed")
    return r.stdout


def run_ffmpeg(args: list[str], duration: float, progress: Callable[[float], None],
               cancel: threading.Event, cwd: Path | None = None) -> None:
    cmd = [ffmpeg_exe(), "-hide_banner", "-nostdin", "-y", "-loglevel", "error",
           "-progress", "pipe:1", "-nostats", *args]
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=cwd,
                            creationflags=NO_WINDOW, text=True, encoding="utf-8", errors="replace")
    err: list[str] = []
    t = threading.Thread(target=lambda: err.extend(proc.stderr), daemon=True)
    t.start()
    try:
        for line in proc.stdout:
            if cancel.is_set():
                proc.kill()
                raise Cancelled()
            if line.startswith("out_time_us=") or line.startswith("out_time_ms="):
                try:
                    us = int(line.split("=", 1)[1])
                    if duration > 0:
                        progress(min(1.0, us / 1e6 / duration))
                except ValueError:
                    pass
        proc.wait()
    finally:
        if proc.poll() is None:
            proc.kill()
    t.join(timeout=2)
    if cancel.is_set():
        raise Cancelled()
    if proc.returncode != 0:
        raise JobError("ffmpeg failed:\n" + "".join(err[-15:]).strip())


def download_section(info: VodInfo, quality: str, start: float, end: float, out: Path,
                     progress: Callable[[float], None], cancel: threading.Event, audio_only: bool = False) -> None:
    dur = end - start
    ss = ["-ss", f"{start:.3f}"]
    if audio_only:
        url = info.streams.get("Audio") or info.streams.get("480p") or info.streams["Source"]
        inputs, maps = [*ss, "-i", url], ["-map", "0:a:0", "-vn"]
    else:
        q = quality if quality in info.streams else "Source"
        url = info.streams[q]
        if q in info.video_only:          # YouTube: picture and sound are separate streams
            inputs = [*ss, "-i", url, *_ua_args(info.user_agent), *ss, "-i", info.streams["Audio"]]
            maps = ["-map", "0:v:0", "-map", "1:a:0"]
        else:
            inputs, maps = [*ss, "-i", url], ["-map", "0:v:0?", "-map", "0:a:0?"]
    # the .mp4/.m4a writer adds the ADTS->ASC fix for AAC by itself (and skips it for Opus)
    run_ffmpeg([*_ua_args(info.user_agent), *inputs, "-t", f"{dur:.3f}",
                *maps, "-c", "copy", "-avoid_negative_ts", "make_zero",
                "-movflags", "+faststart", str(out)],
               dur, progress, cancel)
    if not out.exists() or out.stat().st_size < 10_000:
        raise JobError("The download came back empty. The VOD may be muted, deleted, or sub-only.")


# ---------------------------------------------------------------- Whisper

@dataclass
class Segment:
    start: float
    end: float
    text: str
    speaker: int = 0      # 0 = the person who talks most; others get coloured subtitles


_model_cache: dict[str, object] = {}
_model_device: dict[str, str] = {}
_model_lock = threading.Lock()


MODEL_PATHS: dict[str, str] = {}      # "custom" -> a faster-whisper model folder the user picked


def load_model(name: str, log: Callable[[str], None] = lambda s: None):
    """Load (or reuse) a Whisper model. Two can stay loaded, so live mode and a
    clip job can use different models. Safe to call from several threads."""
    with _model_lock:
        if name in _model_cache:
            return _model_cache[name]
        from . import cuda
        cuda.setup()
        from faster_whisper import WhisperModel
        try:
            model = WhisperModel(MODEL_PATHS.get(name, name), device="cuda", compute_type="float16", num_workers=2)
            _model_device[name] = "GPU"
            log(f"Loaded Whisper {name} on the GPU.")
        except Exception as e:
            log(f"GPU load failed ({e}). Falling back to CPU, which is much slower.")
            model = WhisperModel(MODEL_PATHS.get(name, name), device="cpu", compute_type="int8", num_workers=2)
            _model_device[name] = "CPU"
        while len(_model_cache) >= 2:
            _model_cache.pop(next(iter(_model_cache)))
        _model_cache[name] = model
        return model


def model_device(name: str) -> str | None:
    return _model_device.get(name) if name in _model_cache else None


def model_is_downloaded(name: str) -> bool:
    if name in MODEL_PATHS:
        return Path(MODEL_PATHS[name]).exists()
    try:
        from faster_whisper.utils import download_model
        download_model(name, local_files_only=True)
        return True
    except Exception:
        return False


def decode_audio(path: Path):
    """16 kHz mono float32 samples via ffmpeg (avoids PyAV version mismatches)."""
    import numpy as np
    cmd = [ffmpeg_exe(), "-hide_banner", "-nostdin", "-loglevel", "error", "-i", str(path),
           "-vn", "-ac", "1", "-ar", "16000", "-f", "f32le", "-"]
    r = subprocess.run(cmd, capture_output=True, creationflags=NO_WINDOW)
    if r.returncode != 0:
        raise JobError("Couldn't read the clip's audio:\n" + r.stderr.decode(errors="replace")[-800:])
    audio = np.frombuffer(r.stdout, dtype=np.float32).copy()
    if audio.size < 16000:
        raise JobError("The clip has no audio track.")
    return audio


def detect_language(model, audio, log) -> tuple[str, float, list[tuple[str, float]]]:
    try:
        lang, prob, allp = model.detect_language(audio=audio, vad_filter=True,
                                                 language_detection_segments=8,
                                                 language_detection_threshold=0.7)
    except (TypeError, AttributeError):
        _, info = model.transcribe(audio, vad_filter=True)
        lang, prob, allp = info.language, info.language_probability, info.all_language_probs or []
    top = sorted(allp or [], key=lambda x: -x[1])[:3]
    if top:
        log("Language guess: " + ", ".join(f"{lang_name(c)} {p:.0%}" for c, p in top))
    return lang, prob, top


def _is_junk(seg, text: str) -> bool:
    low = text.lower()
    if any(j in low for j in _JUNK) and len(low) < 80:
        return True
    if getattr(seg, "no_speech_prob", 0) > 0.75 and getattr(seg, "avg_logprob", 0) < -0.9:
        return True
    if re.fullmatch(r"[\W_]*", text):
        return True
    return False


def run_whisper(model, audio, duration: float, language: str, task: str,
                progress: Callable[[float], None], on_segment: Callable[[Segment], None],
                cancel: threading.Event, hotwords: str | None = None,
                fixer: Callable[[str], tuple[str, int]] | None = None) -> tuple[list[Segment], int]:
    segments, _ = model.transcribe(
        audio, language=language, task=task, beam_size=5,
        vad_filter=True, vad_parameters={"min_silence_duration_ms": 500},
        condition_on_previous_text=False, no_speech_threshold=0.6, hotwords=hotwords,
    )
    from .glossary import Glossary
    gl = Glossary.get()
    out: list[Segment] = []
    repeat = 0
    fixes = 0
    for s in segments:
        if cancel.is_set():
            raise Cancelled()
        progress(min(1.0, s.end / duration) if duration else 0)
        text = " ".join(s.text.split())
        if not text or _is_junk(s, text) or gl.is_prompt_echo(text, hotwords):
            continue
        if fixer:
            text, k = fixer(text)
            fixes += k
        if out and text.lower() == out[-1].text.lower():
            repeat += 1
            if repeat >= 2:   # a loop: keep the first two copies only
                continue
        else:
            repeat = 0
        seg = Segment(s.start, s.end, text)
        out.append(seg)
        on_segment(seg)
    progress(1.0)
    return out, fixes


# ---------------------------------------------------------------- subtitle files

def _is_cjk(text: str) -> bool:
    return sum(1 for c in text if ord(c) > 0x2E80) > len(text) * 0.3


def _chunks(text: str, limit: int) -> list[str]:
    if len(text) <= limit:
        return [text]
    if _is_cjk(text):
        return [text[i:i + limit] for i in range(0, len(text), limit)]
    words, out, cur = text.split(), [], ""
    for w in words:
        if cur and len(cur) + 1 + len(w) > limit:
            out.append(cur)
            cur = w
        else:
            cur = f"{cur} {w}".strip()
    if cur:
        out.append(cur)
    return out


def _balanced_wrap(text: str, line_len: int) -> str:
    """Two lines of similar length instead of one full line plus a stray word."""
    if len(text) <= line_len:
        return text
    for width in range(max(len(text) // 2, 12), line_len + 1):
        lines = textwrap.wrap(text, width)
        if len(lines) <= 2:
            return "\n".join(lines)
    return "\n".join(textwrap.wrap(text, line_len)[:2])


def to_cues(segments: list[Segment]) -> list[Segment]:
    """Split long Whisper segments into readable 2-line cues with proportional timing."""
    cues: list[Segment] = []
    for seg in segments:
        cjk = _is_cjk(seg.text)
        line_len = 22 if cjk else 42
        parts = _chunks(seg.text, line_len * 2)
        total = sum(len(p) for p in parts) or 1
        t = seg.start
        for p in parts:
            d = (seg.end - seg.start) * len(p) / total
            wrapped = p if cjk else _balanced_wrap(p, line_len)
            if cjk and len(p) > line_len:
                wrapped = p[:line_len] + "\n" + p[line_len:]
            cues.append(Segment(t, t + d, wrapped, seg.speaker))
            t += d
    # minimum display time without overlapping the next cue
    for i, c in enumerate(cues):
        nxt = cues[i + 1].start if i + 1 < len(cues) else c.end + 2
        if c.end - c.start < 1.2:
            c.end = min(c.start + 1.2, max(c.end, nxt - 0.05))
    return cues


def _colored(text: str, speaker: int) -> str:
    from .speakers import color
    c = color(speaker)
    return f'<font color="{c}">{text}</font>' if c else text


def write_srt(segments: list[Segment], path: Path, shift: float = 0.0) -> None:
    """shift: seconds added to every time (to line subtitles up with a video that starts earlier)."""
    lines = []
    for i, c in enumerate(to_cues(segments), 1):
        lines += [str(i), f"{srt_ts(max(0.0, c.start + shift))} --> {srt_ts(max(0.0, c.end + shift))}",
                  _colored(c.text, c.speaker), ""]
    path.write_text("\n".join(lines), encoding="utf-8")


def audio_offset(ref, other, max_s: float = 15.0) -> float:
    """Seconds d such that other[t + d] matches ref[t] (both 16 kHz mono). Uses the first minute."""
    import numpy as np
    n = min(len(ref), len(other), 16000 * 60)
    if n < 16000 * 3:
        return 0.0
    a = ref[:n] - ref[:n].mean()
    b = other[:n] - other[:n].mean()
    size = 1 << (2 * n - 1).bit_length()
    c = np.fft.irfft(np.fft.rfft(b, size) * np.conj(np.fft.rfft(a, size)), size)
    m = int(max_s * 16000)
    cand = np.concatenate([c[:m], c[-m:]])
    k = int(np.argmax(cand))
    return (k if k < m else k - 2 * m) / 16000


def write_transcript(path: Path, info: VodInfo, start: float, end: float, lang: str, prob: float,
                     model_name: str, english: list[Segment], original: list[Segment] | None,
                     clip_shift: float = 0.0) -> None:
    """clip_shift: how much later a moment appears in the saved video than in the VOD section."""
    many = len({s.speaker for s in english}) > 1

    def block(segs: list[Segment]) -> list[str]:
        return [f"[VOD {format_ts(start + s.start):>8} | clip {format_ts(s.start + clip_shift):>6}]  "
                f"{f'Speaker {s.speaker + 1}: ' if many else ''}{s.text}" for s in segs]

    head = [
        info.title,
        f"Channel: {info.channel}",
        f"VOD: {info.url}",
        f"Section: {format_ts(start)} - {format_ts(end)}   ({vod_link(info, start)})",
        f"Detected language: {lang_name(lang)} ({prob:.0%})   Model: whisper {model_name}",
        "",
    ]
    body = ["=== English ===", *block(english)] if english else ["(no speech found)"]
    if original:
        body += ["", f"=== Original ({lang_name(lang)}) ===", *block(original)]
    path.write_text("\n".join(head + body) + "\n", encoding="utf-8")


# ---------------------------------------------------------------- burn-in

def burn_subtitles(clip: Path, srt: Path, out: Path, position: str, font_size: str, duration: float,
                   progress: Callable[[float], None], log: Callable[[str], None],
                   cancel: threading.Event) -> None:
    align, margin = POSITIONS.get(position, POSITIONS["Above game HUD"])
    size = FONT_SIZES.get(font_size, 14)
    style = (f"FontName=Arial,FontSize={size},Bold=1,PrimaryColour=&H00FFFFFF,"
             f"OutlineColour=&H00000000,BackColour=&H80000000,BorderStyle=1,Outline=1.6,"
             f"Shadow=0.6,Alignment={align},MarginV={margin}")
    # Run inside the output folder with a plain file name: avoids ffmpeg's
    # filter-path escaping problems with drive letters and odd characters.
    tmp = clip.parent / "_burn.srt"
    shutil.copyfile(srt, tmp)
    vf = f"subtitles=_burn.srt:force_style='{style}'"
    common_in = ["-i", clip.name, "-vf", vf]
    tail = ["-pix_fmt", "yuv420p", "-c:a", "copy", "-movflags", "+faststart", out.name]
    try:
        try:
            run_ffmpeg([*common_in, "-c:v", "h264_nvenc", "-preset", "p5", "-tune", "hq",
                        "-rc", "vbr", "-cq", "21", "-b:v", "0", *tail],
                       duration, progress, cancel, cwd=clip.parent)
        except JobError as e:
            log(f"GPU encoder unavailable, using CPU encoder. ({str(e).splitlines()[-1][:120]})")
            run_ffmpeg([*common_in, "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", *tail],
                       duration, progress, cancel, cwd=clip.parent)
    finally:
        tmp.unlink(missing_ok=True)


# ---------------------------------------------------------------- the job

@dataclass
class JobParams:
    url: str
    start: float
    end: float
    out_dir: Path
    quality: str = "720p"
    model: str = "large-v3"
    language: str | None = None        # None = auto-detect
    want_original: bool = False
    burn: bool = True
    position: str = "Above game HUD"
    font_size: str = "Medium"
    champions: list[str] = field(default_factory=list)   # champions in this game (glossary priority)
    use_glossary: bool = True
    isolate: bool = False          # strip game audio/music before Whisper
    label: str = ""                # shown in the queue / history
    method: str = "two-step"       # "two-step" (Whisper writes it down, NLLB translates) or "whisper"
    translator: str = "better"     # NLLB size for two-step: "standard" or "better"
    auto_details: bool = True      # read champions/player names off the video for the glossary
    speakers: bool = False         # tell speakers apart and colour the others' subtitles


@dataclass
class Callbacks:
    stage: Callable[[int, int, str], None] = lambda i, n, label: None
    progress: Callable[[float], None] = lambda f: None   # 0..1, or -1 for "busy"
    log: Callable[[str], None] = lambda s: None
    segment: Callable[[Segment, str], None] = lambda seg, kind: None
    cancel: threading.Event = field(default_factory=threading.Event)


@dataclass
class JobResult:
    folder: Path
    clip: Path
    srt: Path
    transcript: Path
    subbed: Path | None
    language: str
    probability: float
    segments: int
    seconds: float
    meta: dict = field(default_factory=dict)


def _safe(name: str) -> str:
    return re.sub(r"[^\w.-]+", "_", name, flags=re.UNICODE).strip("_")[:40] or "stream"


class _Either:
    """Cancelled when either event is set (the job's cancel, or 'stop the background download')."""
    def __init__(self, *events):
        self.events = events

    def is_set(self) -> bool:
        return any(e.is_set() for e in self.events)


DL_FILE = "download.json"


def _downloads(folder: Path) -> dict:
    import json
    try:
        return json.loads((folder / DL_FILE).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}


def _remember_download(folder: Path, kind: str, file: Path, quality: str | None = None):
    import json
    d = _downloads(folder)
    d[kind] = {"file": file.name, "quality": quality, "size": file.stat().st_size, "at": time.time()}
    (folder / DL_FILE).write_text(json.dumps(d, indent=2), encoding="utf-8")


def _cached(folder: Path, kind: str, quality: str | None = None) -> Path | None:
    """A finished download of this section from an earlier job (same file size, same quality)."""
    e = _downloads(folder).get(kind)
    if not e:
        return None
    f = folder / e["file"]
    if not f.exists() or f.stat().st_size != e.get("size"):
        return None
    if quality and e.get("quality") != quality:
        return None
    return f


def run_job(p: JobParams, cb: Callbacks) -> JobResult:
    t0 = time.time()
    if p.end - p.start < 2:
        raise JobError("Pick a section at least a few seconds long.")

    two_step = p.method == "two-step"
    need_video = p.burn
    stages = ["Reading VOD info", "Downloading audio", "Loading Whisper"]
    if p.isolate:
        stages.append("Isolating voices")
    stages.append("Detecting language")
    if two_step:
        stages += ["Transcribing speech", "Translating to English"]
    else:
        stages.append("Translating to English")
        if p.want_original:
            stages.append("Transcribing original language")
    if p.speakers:
        stages.append("Telling speakers apart")
    if need_video:
        stages += ["Downloading video", "Burning subtitles into video"]
    n = len(stages)
    step = 0

    def stage(label: str):
        nonlocal step
        step += 1
        cb.stage(step, n, label)
        cb.progress(-1)

    def check():
        if cb.cancel.is_set():
            raise Cancelled()

    stage(stages[0])
    info = fetch_info(p.url)          # always re-read: Twitch playlist tokens expire
    end = min(p.end, info.duration) if info.duration else p.end
    start = max(0.0, p.start)
    dur = end - start
    check()

    base = f"{_safe(info.channel)}_{_safe(info.id) or 'vod'}_{format_hms_tag(start)}-{format_hms_tag(end)}"
    folder = Path(p.out_dir) / base
    folder.mkdir(parents=True, exist_ok=True)
    audio_file = folder / f"{base}.m4a"
    video_file = folder / f"{base}.mp4"
    cb.log(f"Saving to {folder}")

    # The video downloads in the background while Whisper works on the sound.
    stop_video = threading.Event()
    video_err: list[BaseException] = []
    vprog = [0.0]
    video_thread = None
    have_video = _cached(folder, "video", p.quality) if need_video else None
    if need_video and have_video:
        cb.log(f"Reusing the {p.quality} video downloaded earlier.")
    elif need_video:
        def _video():
            try:
                download_section(info, p.quality, start, end, video_file, lambda f: vprog.__setitem__(0, f),
                                 _Either(cb.cancel, stop_video))
                _remember_download(folder, "video", video_file, p.quality)
            except BaseException as e:          # noqa: BLE001 - reported when the job needs the video
                video_err.append(e)
        video_thread = threading.Thread(target=_video, daemon=True)
        video_thread.start()

    # champions and player names, read off the video while it downloads
    details: dict = {}
    det_thread = None
    det_file = folder / "details.json"
    if p.auto_details and p.use_glossary:
        import json
        try:
            details.update(json.loads(det_file.read_text(encoding="utf-8")))      # read on an earlier run
        except (OSError, ValueError):
            def _detect():
                try:
                    from .detect import game_details
                    found = game_details(info, start, end, cb.cancel)
                    details.update(found)
                    if not cb.cancel.is_set():
                        det_file.write_text(json.dumps(found, ensure_ascii=False), encoding="utf-8")
                except Exception as e:
                    details["error"] = str(e)
            det_thread = threading.Thread(target=_detect, daemon=True)
            det_thread.start()

    try:
        stage("Downloading audio")
        if _cached(folder, "audio"):
            cb.log("Reusing the audio downloaded earlier.")
        else:
            cb.log(f"Downloading {format_ts(start)} - {format_ts(end)} ({format_ts(dur)}): sound first"
                   + (f", the {p.quality} video in the background." if video_thread else "."))
            download_section(info, p.quality, start, end, audio_file, cb.progress, cb.cancel, audio_only=True)
            _remember_download(folder, "audio", audio_file)

        thumb = folder / "thumb.jpg"
        if not thumb.exists():
            try:
                thumb.write_bytes(grab_frame(info, start + dur * 0.3, 480))
            except Exception:
                pass

        stage("Loading Whisper")
        if not model_is_downloaded(p.model):
            cb.log(f"First run: downloading the Whisper {p.model} model. This only happens once.")
        model = load_model(p.model, cb.log)
        check()

        raw = decode_audio(audio_file)
        if p.isolate:
            stage("Isolating voices")
            from . import isolate
            if not isolate.model_ready():
                cb.log("First use: downloading the voice isolation model (66 MB).")
                isolate.download_model(cb.progress)
            try:
                audio = isolate.voice_16k_from_file(audio_file, cb.progress, cb.cancel)
                cb.log(f"Game sound and music removed ({isolate.device()}).")
            finally:
                isolate.release()     # give the GPU memory back to Whisper
        else:
            audio = raw
        audio_dur = len(audio) / 16000 or dur

        stage("Detecting language")
        if p.language:
            lang, prob = p.language, 1.0
            cb.log(f"Language set manually: {lang_name(lang)}")
        else:
            lang, prob, _ = detect_language(model, audio, cb.log)
            cb.log(f"Detected language: {lang_name(lang)} ({prob:.0%})")
            if prob < 0.5:
                cb.log("Low confidence. If the result looks wrong, pick the language manually and run again.")
        check()

        from .glossary import Glossary
        gl = Glossary.get()
        if det_thread:
            det_thread.join(timeout=20)          # don't hold up a short clip; long ones finish downloading first
            if det_thread.is_alive():
                cb.log("Still reading champions/players from the video; skipping that for this clip.")
        if details.get("champions") or details.get("players"):
            cb.log("Read from the game: " + ", ".join(details.get("champions", []) + details.get("players", [])))
        champs = list(dict.fromkeys(list(p.champions) + details.get("champions", [])))
        names = details.get("players", [])
        fix_en = (lambda t: gl.fix(t, english=True, extra=names)) if p.use_glossary else None
        fix_orig = (lambda t: gl.fix(t, english=False, extra=names)) if p.use_glossary else None

        from . import translate
        if two_step and lang != "en" and not translate.supported(lang):
            cb.log(f"The text translator doesn't cover {lang_name(lang)}, so Whisper translates directly.")
            two_step = False

        original: list[Segment] | None = None
        fixes = slang = 0
        if two_step:
            stage("Transcribing speech")
            hot = gl.hotwords(champs, core=False, extra=names) if p.use_glossary else None
            original, _ = run_whisper(model, audio, audio_dur, lang, "transcribe", cb.progress,
                                      lambda s: cb.segment(s, "orig"), cb.cancel, hotwords=hot, fixer=fix_orig)
            check()
            stage("Translating to English")
            if lang == "en":
                english, original = original, None
            else:
                tr = translate.Translator.get(p.translator, cb.log)
                src = []
                for s in original:
                    t, k = gl.prepare(s.text, lang) if p.use_glossary else (s.text, 0)
                    slang += k
                    src.append(t)
                outs = tr.translate(src, lang)
                english = []
                for s, t in zip(original, outs):
                    if fix_en:
                        t, k = fix_en(t)
                        fixes += k
                    seg = Segment(s.start, s.end, t)
                    english.append(seg)
                    cb.segment(seg, "en")
                cb.progress(1.0)
                cb.log(f"Translated by Whisper {p.model} + NLLB {'1.3B' if tr.size == 'better' else '600M'} ({tr.device}).")
        else:
            hot = gl.hotwords(champs, extra=names) if p.use_glossary else None
            if hot:
                cb.log(f"League glossary on ({len(hot.split(','))} terms in Whisper's prompt).")
            stage("Translating to English")
            task = "transcribe" if lang == "en" else "translate"
            if lang == "en":
                cb.log("Speech is already English, so it's transcribed without translating.")
            english, fixes = run_whisper(model, audio, audio_dur, lang, task, cb.progress,
                                         lambda s: cb.segment(s, "en"), cb.cancel, hotwords=hot, fixer=fix_en)
            if p.want_original:
                stage("Transcribing original language")
                original, _ = run_whisper(model, audio, audio_dur, lang, "transcribe", cb.progress,
                                          lambda s: cb.segment(s, "orig"), cb.cancel, fixer=fix_orig)
        cb.log(f"{len(english)} English lines." + (f" Glossary fixed {fixes} terms." if fixes else "")
               + (f" {slang} slang phrases explained before translating." if slang else ""))

        n_speakers = 1
        if p.speakers and english:
            stage("Telling speakers apart")
            from . import speakers
            if not speakers.model_ready():
                cb.log("First use: downloading the speaker model (26 MB).")
                speakers.download_model(cb.progress)
            ids = speakers.label(audio, [(s.start, s.end) for s in english], cb.progress, cb.cancel)
            for s, k in zip(english, ids):
                s.speaker = k
            if original and len(original) == len(english):
                for s, k in zip(original, ids):
                    s.speaker = k
            n_speakers = len(set(ids))
            cb.log(f"{n_speakers} speakers." + (" Others are shown in colour; the main speaker in white."
                                                if n_speakers > 1 else " Everything was said by one person."))
            if n_speakers == 1:
                for s in english + (original or []):
                    s.speaker = 0

        # The video can start a few seconds before the requested time (it has to begin on a keyframe).
        # Find how much earlier by matching its sound against the audio we transcribed.
        shift = 0.0
        if need_video:
            stage("Downloading video")
            if video_thread:
                if video_thread.is_alive():
                    cb.log("Waiting for the video download to finish…")
                while video_thread.is_alive():
                    cb.progress(vprog[0])
                    check()
                    video_thread.join(timeout=0.3)
                if video_err:
                    raise video_err[0]
            cb.progress(1.0)
            try:
                shift = round(audio_offset(raw, decode_audio(video_file)), 3)
            except JobError:
                shift = 0.0
            if abs(shift) >= 0.05:
                cb.log(f"The video starts {abs(shift):.1f} s {'earlier' if shift > 0 else 'later'}; subtitles moved to match.")
            else:
                shift = 0.0

        if original and lang != "en":
            write_srt(original, folder / f"{base}.{lang}.srt", shift)
        srt = folder / f"{base}.en.srt"
        write_srt(english, srt, shift)
        transcript = folder / f"{base}.transcript.txt"
        write_transcript(transcript, info, start, end, lang, prob, p.model, english,
                         original if (p.want_original or two_step) and lang != "en" else None, clip_shift=shift)

        clip = video_file if need_video else audio_file
        meta = {"vod_url": info.url, "vod_id": info.id, "site": info.site, "title": info.title, "channel": info.channel,
                "start": start, "end": end, "language": lang, "probability": prob, "model": p.model,
                "method": "two-step" if two_step else "whisper", "translator": p.translator if two_step else None,
                "position": p.position, "size": p.font_size, "lines": len(english), "created": time.time(),
                "clip": clip.name, "audio_only": not need_video, "srt": srt.name, "transcript": transcript.name,
                "thumb": thumb.name if thumb.exists() else None, "subbed": None, "shift": shift,
                "speakers": n_speakers, "champions": champs, "players": names}
        write_meta(folder, meta)

        subbed = None
        if need_video:
            stage("Burning subtitles into video")
            subbed = folder / f"{base}.subbed.mp4"
            burn_subtitles(video_file, srt, subbed, p.position, p.font_size, audio_dur + shift,
                           cb.progress, cb.log, cb.cancel)
            meta["subbed"] = subbed.name
            write_meta(folder, meta)
    finally:
        if video_thread and video_thread.is_alive():
            stop_video.set()                  # the job failed or was cancelled: stop the background download
            video_thread.join(timeout=5)
            video_file.unlink(missing_ok=True)

    cb.progress(1.0)
    return JobResult(folder, clip, srt, transcript, subbed, lang, prob, len(english), time.time() - t0, meta)


# ---------------------------------------------------------------- saved job details

def write_meta(folder: Path, meta: dict):
    import json
    (Path(folder) / "job.json").write_text(json.dumps(meta, indent=2, ensure_ascii=False), encoding="utf-8")


def read_meta(folder: Path) -> dict:
    import json
    return json.loads((Path(folder) / "job.json").read_text(encoding="utf-8"))


_FONT = re.compile(r'<font color="(#[0-9A-Fa-f]{6})">(.*)</font>', re.S)


def read_srt(path: Path) -> list[dict]:
    cues = []
    blocks = re.split(r"\n\s*\n", Path(path).read_text(encoding="utf-8-sig").replace("\r\n", "\n").strip())
    rx = re.compile(r"(\d+):(\d+):(\d+)[,.](\d+)\s*-->\s*(\d+):(\d+):(\d+)[,.](\d+)")
    for b in blocks:
        lines = b.split("\n")
        for i, ln in enumerate(lines[:2]):
            m = rx.search(ln)
            if m:
                g = [int(x) for x in m.groups()]
                text = "\n".join(lines[i + 1:]).strip()
                fm = _FONT.fullmatch(text)          # a speaker colour: kept separately so the editor shows plain text
                cue = {"start": g[0] * 3600 + g[1] * 60 + g[2] + g[3] / 1000,
                       "end": g[4] * 3600 + g[5] * 60 + g[6] + g[7] / 1000,
                       "text": fm.group(2).strip() if fm else text}
                if fm:
                    cue["color"] = fm.group(1)
                cues.append(cue)
                break
    return cues


def write_srt_cues(cues: list[dict], path: Path):
    """Write cues exactly as edited (no re-splitting)."""
    keep = [c for c in sorted(cues, key=lambda c: c["start"]) if (c.get("text") or "").strip()]
    out = []
    for i, c in enumerate(keep, 1):
        text = c["text"].strip()
        if c.get("color"):
            text = f'<font color="{c["color"]}">{text}</font>'
        out += [str(i), f"{srt_ts(c['start'])} --> {srt_ts(max(c['end'], c['start'] + 0.2))}", text, ""]
    Path(path).write_text("\n".join(out), encoding="utf-8")


def reburn(folder: Path, position: str, size: str, cb: Callbacks) -> Path:
    """Burn the (edited) subtitles into the clip again, without re-translating."""
    folder = Path(folder)
    meta = read_meta(folder)
    clip, srt = folder / meta["clip"], folder / meta["srt"]
    if meta.get("audio_only"):
        raise JobError("This job only downloaded the audio (subtitles only), so there's no video to burn into. "
                       "Translate the section again with 'Burn subtitles into video' on.")
    if not clip.exists():
        raise JobError("The original clip file is missing, so the subtitles can't be burned again.")
    cb.stage(1, 1, "Burning subtitles into video")
    out = folder / (clip.stem + ".subbed.mp4")
    burn_subtitles(clip, srt, out, position, size, max(1.0, meta["end"] - meta["start"]), cb.progress, cb.log, cb.cancel)
    meta.update(subbed=out.name, position=position, size=size, edited=time.time())
    write_meta(folder, meta)
    return out
