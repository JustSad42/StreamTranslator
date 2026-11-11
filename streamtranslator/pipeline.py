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

    def to_dict(self) -> dict:
        return {"url": self.url, "id": self.id, "title": self.title, "channel": self.channel,
                "duration": self.duration, "url_start": self.url_start, "start_ts": self.start_ts,
                "thumbnail": self.thumbnail, "storyboard": self.storyboard}


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


def fetch_info(url: str) -> VodInfo:
    import yt_dlp

    url = url.strip()
    if not url:
        raise JobError("Paste a Twitch VOD link first.")
    if url.isdigit():
        url = f"https://www.twitch.tv/videos/{url}"
    opts = {"quiet": True, "no_warnings": True, "skip_download": True,
            "logger": _QuietLogger(), "noprogress": True}
    try:
        with yt_dlp.YoutubeDL(opts) as ydl:
            info = ydl.extract_info(url, download=False)
    except Exception as e:  # yt-dlp raises DownloadError with a readable message
        msg = re.sub(r"\x1b\[[0-9;]*m", "", str(e)).replace("ERROR: ", "")
        raise JobError(f"Couldn't read that VOD.\n{msg}\n\n"
                       "Subscriber-only VODs can't be downloaded without logging in.") from None

    if info.get("_type") == "playlist":
        raise JobError("That link is a channel or playlist. Paste a single VOD link "
                       "(twitch.tv/videos/123456789).")
    if info.get("is_live"):
        raise JobError("That stream is live. Wait for the VOD, then use the VOD link.")

    vids = [f for f in info.get("formats") or []
            if f.get("url") and f.get("vcodec") not in (None, "none") and f.get("height")]
    if not vids:
        raise JobError("No playable video formats found for that link.")
    vids.sort(key=lambda f: (f.get("height") or 0, f.get("fps") or 0, f.get("tbr") or 0))

    def best_under(h: int):
        under = [f for f in vids if f["height"] <= h]
        return (under or vids[:1])[-1]

    streams = {"Source": vids[-1]["url"], "720p": best_under(720)["url"], "480p": best_under(480)["url"]}
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
        url=info.get("webpage_url") or url,
        id=str(info.get("id") or "").lstrip("v"),
        title=info.get("title") or info.get("description") or "Untitled",
        channel=info.get("uploader") or info.get("uploader_id") or info.get("channel") or "streamer",
        duration=float(info.get("duration") or 0),
        streams=streams,
        preview_stream=preview,
        user_agent=ua,
        url_start=timestamp_from_url(url),
    )


def twitch_link(info: VodInfo, seconds: float) -> str:
    if "twitch.tv" in info.url and info.id:
        return f"https://www.twitch.tv/videos/{info.id}?t={format_hms_tag(seconds)}"
    return info.url


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
                     progress: Callable[[float], None], cancel: threading.Event) -> None:
    url = info.streams.get(quality) or info.streams["Source"]
    dur = end - start
    run_ffmpeg([*_ua_args(info.user_agent), "-ss", f"{start:.3f}", "-i", url, "-t", f"{dur:.3f}",
                "-map", "0:v:0?", "-map", "0:a:0?", "-c", "copy",
                "-bsf:a", "aac_adtstoasc", "-avoid_negative_ts", "make_zero",
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


_model_cache: dict[str, object] = {}
_model_device: dict[str, str] = {}
_model_lock = threading.Lock()


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
            model = WhisperModel(name, device="cuda", compute_type="float16", num_workers=2)
            _model_device[name] = "GPU"
            log(f"Loaded Whisper {name} on the GPU.")
        except Exception as e:
            log(f"GPU load failed ({e}). Falling back to CPU, which is much slower.")
            model = WhisperModel(name, device="cpu", compute_type="int8", num_workers=2)
            _model_device[name] = "CPU"
        while len(_model_cache) >= 2:
            _model_cache.pop(next(iter(_model_cache)))
        _model_cache[name] = model
        return model


def model_device(name: str) -> str | None:
    return _model_device.get(name) if name in _model_cache else None


def model_is_downloaded(name: str) -> bool:
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
            cues.append(Segment(t, t + d, wrapped))
            t += d
    # minimum display time without overlapping the next cue
    for i, c in enumerate(cues):
        nxt = cues[i + 1].start if i + 1 < len(cues) else c.end + 2
        if c.end - c.start < 1.2:
            c.end = min(c.start + 1.2, max(c.end, nxt - 0.05))
    return cues


def write_srt(segments: list[Segment], path: Path) -> None:
    lines = []
    for i, c in enumerate(to_cues(segments), 1):
        lines += [str(i), f"{srt_ts(c.start)} --> {srt_ts(c.end)}", c.text, ""]
    path.write_text("\n".join(lines), encoding="utf-8")


def write_transcript(path: Path, info: VodInfo, start: float, end: float, lang: str, prob: float,
                     model_name: str, english: list[Segment], original: list[Segment] | None) -> None:
    def block(segs: list[Segment]) -> list[str]:
        return [f"[VOD {format_ts(start + s.start):>8} | clip {format_ts(s.start):>6}]  {s.text}" for s in segs]

    head = [
        info.title,
        f"Channel: {info.channel}",
        f"VOD: {info.url}",
        f"Section: {format_ts(start)} - {format_ts(end)}   ({twitch_link(info, start)})",
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


def run_job(p: JobParams, cb: Callbacks) -> JobResult:
    t0 = time.time()
    if p.end - p.start < 2:
        raise JobError("Pick a section at least a few seconds long.")

    stages = ["Reading VOD info", "Downloading section", "Loading Whisper"]
    if p.isolate:
        stages.append("Isolating voices")
    stages += ["Detecting language", "Translating to English"]
    if p.want_original:
        stages.append("Transcribing original language")
    if p.burn:
        stages.append("Burning subtitles into video")
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

    base = f"{_safe(info.channel)}_{info.id or 'vod'}_{format_hms_tag(start)}-{format_hms_tag(end)}"
    folder = Path(p.out_dir) / base
    folder.mkdir(parents=True, exist_ok=True)
    clip = folder / f"{base}.mp4"
    cb.log(f"Saving to {folder}")

    stage(stages[1])
    cb.log(f"Downloading {format_ts(start)} - {format_ts(end)} ({format_ts(dur)}) at {p.quality}...")
    download_section(info, p.quality, start, end, clip, cb.progress, cb.cancel)

    thumb = folder / "thumb.jpg"
    subprocess.run([ffmpeg_exe(), "-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-ss", f"{dur * 0.3:.1f}",
                    "-i", str(clip), "-frames:v", "1", "-vf", "scale=480:-2", str(thumb)],
                   capture_output=True, creationflags=NO_WINDOW)

    stage("Loading Whisper")
    if not model_is_downloaded(p.model):
        cb.log(f"First run: downloading the Whisper {p.model} model (about 3 GB for large-v3). "
               "This only happens once.")
    model = load_model(p.model, cb.log)
    check()

    if p.isolate:
        stage("Isolating voices")
        from . import isolate
        if not isolate.model_ready():
            cb.log("First use: downloading the voice isolation model (66 MB).")
            isolate.download_model(cb.progress)
        try:
            audio = isolate.voice_16k_from_file(clip, cb.progress, cb.cancel)
            cb.log(f"Game sound and music removed ({isolate.device()}).")
        finally:
            isolate.release()     # give the GPU memory back to Whisper
    else:
        audio = decode_audio(clip)
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
    hot = gl.hotwords(p.champions) if p.use_glossary else None
    if hot:
        cb.log(f"League glossary on ({len(hot.split(','))} terms in Whisper's prompt).")

    stage("Translating to English")
    task = "transcribe" if lang == "en" else "translate"
    if lang == "en":
        cb.log("Speech is already English, so it's transcribed without translating.")
    english, fixes = run_whisper(model, audio, audio_dur, lang, task, cb.progress,
                                 lambda s: cb.segment(s, "en"), cb.cancel, hotwords=hot,
                                 fixer=(lambda t: gl.fix(t, english=True)) if p.use_glossary else None)
    cb.log(f"{len(english)} English lines." + (f" Glossary fixed {fixes} terms." if fixes else ""))

    original = None
    if p.want_original:
        stage("Transcribing original language")
        original, _ = run_whisper(model, audio, audio_dur, lang, "transcribe", cb.progress,
                                  lambda s: cb.segment(s, "orig"), cb.cancel,
                                  fixer=(lambda t: gl.fix(t, english=False)) if p.use_glossary else None)
        write_srt(original, folder / f"{base}.{lang}.srt")

    srt = folder / f"{base}.en.srt"
    write_srt(english, srt)
    transcript = folder / f"{base}.transcript.txt"
    write_transcript(transcript, info, start, end, lang, prob, p.model, english, original)

    meta = {"vod_url": info.url, "vod_id": info.id, "title": info.title, "channel": info.channel,
            "start": start, "end": end, "language": lang, "probability": prob, "model": p.model,
            "position": p.position, "size": p.font_size, "lines": len(english), "created": time.time(),
            "clip": clip.name, "srt": srt.name, "transcript": transcript.name,
            "thumb": thumb.name if thumb.exists() else None, "subbed": None}
    write_meta(folder, meta)

    subbed = None
    if p.burn:
        stage("Burning subtitles into video")
        subbed = folder / f"{base}.subbed.mp4"
        burn_subtitles(clip, srt, subbed, p.position, p.font_size, audio_dur,
                       cb.progress, cb.log, cb.cancel)
        meta["subbed"] = subbed.name
        write_meta(folder, meta)

    cb.progress(1.0)
    return JobResult(folder, clip, srt, transcript, subbed, lang, prob, len(english), time.time() - t0, meta)


# ---------------------------------------------------------------- saved job details

def write_meta(folder: Path, meta: dict):
    import json
    (Path(folder) / "job.json").write_text(json.dumps(meta, indent=2, ensure_ascii=False), encoding="utf-8")


def read_meta(folder: Path) -> dict:
    import json
    return json.loads((Path(folder) / "job.json").read_text(encoding="utf-8"))


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
                cues.append({"start": g[0] * 3600 + g[1] * 60 + g[2] + g[3] / 1000,
                             "end": g[4] * 3600 + g[5] * 60 + g[6] + g[7] / 1000,
                             "text": "\n".join(lines[i + 1:]).strip()})
                break
    return cues


def write_srt_cues(cues: list[dict], path: Path):
    """Write cues exactly as edited (no re-splitting)."""
    keep = [c for c in sorted(cues, key=lambda c: c["start"]) if (c.get("text") or "").strip()]
    out = []
    for i, c in enumerate(keep, 1):
        out += [str(i), f"{srt_ts(c['start'])} --> {srt_ts(max(c['end'], c['start'] + 0.2))}", c["text"].strip(), ""]
    Path(path).write_text("\n".join(out), encoding="utf-8")


def reburn(folder: Path, position: str, size: str, cb: Callbacks) -> Path:
    """Burn the (edited) subtitles into the clip again, without re-translating."""
    folder = Path(folder)
    meta = read_meta(folder)
    clip, srt = folder / meta["clip"], folder / meta["srt"]
    if not clip.exists():
        raise JobError("The original clip file is missing, so the subtitles can't be burned again.")
    cb.stage(1, 1, "Burning subtitles into video")
    out = folder / (clip.stem + ".subbed.mp4")
    burn_subtitles(clip, srt, out, position, size, max(1.0, meta["end"] - meta["start"]), cb.progress, cb.log, cb.cancel)
    meta.update(subbed=out.name, position=position, size=size, edited=time.time())
    write_meta(folder, meta)
    return out
