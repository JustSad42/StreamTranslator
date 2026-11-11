"""Snap a section to the exact League game using the in-game clock.

Frames are read with Windows' built-in OCR (on-device). Any "mm:ss" seen at VOD
time T votes for "the game started at T - mm:ss". The in-game clock is the only
on-screen clock whose votes agree across frames (a wall clock or stream uptime
won't), so the biggest cluster of agreeing votes is the game start. The end is
found by walking forward until that clock disappears, then narrowing it down.
"""
from __future__ import annotations

import asyncio
import re
import statistics
import subprocess
import threading
from concurrent.futures import ThreadPoolExecutor
from typing import Callable

from .pipeline import NO_WINDOW, Cancelled, JobError, VodInfo, ffmpeg_exe

TOL = 3.0                 # seconds two votes may differ and still agree
_CLOCK = re.compile(r"(?<![\d:])(\d{1,2})[:;.](\d{2})(?![\d:])")
_FIX = str.maketrans({"O": "0", "o": "0", "D": "0", "l": "1", "I": "1", "|": "1", "S": "5", "B": "8", "Z": "2"})


# ---------------------------------------------------------------- OCR

_engine = None
_engine_lock = threading.Lock()


def _get_engine():
    global _engine
    with _engine_lock:
        if _engine is None:
            from winrt.windows.globalization import Language
            from winrt.windows.media.ocr import OcrEngine
            _engine = OcrEngine.try_create_from_language(Language("en-US")) or \
                OcrEngine.try_create_from_user_profile_languages()
            if _engine is None:
                raise JobError("Windows OCR isn't available. Add an English language pack in Windows Settings.")
        return _engine


async def _ocr_async(png: bytes) -> list[str]:
    from winrt.windows.graphics.imaging import BitmapDecoder
    from winrt.windows.storage.streams import DataWriter, InMemoryRandomAccessStream
    stream = InMemoryRandomAccessStream()
    writer = DataWriter(stream)
    writer.write_bytes(png)
    await writer.store_async()
    await writer.flush_async()
    writer.detach_stream()
    stream.seek(0)
    decoder = await BitmapDecoder.create_async(stream)
    bitmap = await decoder.get_software_bitmap_async()
    result = await _get_engine().recognize_async(bitmap)
    return [line.text for line in result.lines]


_ocr_lock = threading.Lock()


def ocr(png: bytes) -> list[str]:
    with _ocr_lock:       # the Windows OCR engine handles one image at a time
        return asyncio.run(_ocr_async(png))


def clock_values(lines: list[str]) -> list[int]:
    """Every plausible in-game time (seconds) found in OCR text."""
    out = []
    for line in lines:
        fixed = " ".join(w.translate(_FIX) if sum(c.isdigit() for c in w) >= 2 else w for w in line.split())
        for m in _CLOCK.finditer(fixed):
            mm, ss = int(m.group(1)), int(m.group(2))
            if ss < 60 and mm < 80:
                out.append(mm * 60 + ss)
    return out


# Digits OCR commonly swaps on game fonts (a "7" read as "1", "6" as "8"...).
_CONFUSE = {"1": "7", "7": "1", "6": "85", "8": "630", "5": "6", "3": "8", "0": "8"}


def _minute_variants(g: int) -> list[int]:
    """The reading plus versions with one minute digit swapped. Seconds tick
    every frame so they cross-check themselves; minutes are where a misread hurts."""
    mm, ss = divmod(g, 60)
    s = f"{mm:02d}"
    out = {g}
    for i, ch in enumerate(s):
        for alt in _CONFUSE.get(ch, ""):
            v = int(s[:i] + alt + s[i + 1:])
            if v < 80:
                out.add(v * 60 + ss)
    return sorted(out)


# ---------------------------------------------------------------- frames

def _source_stream(info: VodInfo) -> str:
    return info.streams.get("Source") or info.streams.get("720p") or info.preview_stream


def top_strip(info: VodInfo, t: float) -> bytes:
    """Top 22% of the frame (where League shows the game clock), upscaled for OCR."""
    cmd = [ffmpeg_exe(), "-hide_banner", "-nostdin", "-loglevel", "error"]
    if info.user_agent:
        cmd += ["-user_agent", info.user_agent]
    cmd += ["-ss", f"{max(0.0, t):.2f}", "-i", _source_stream(info), "-frames:v", "1",
            # dark-on-light, high contrast, enlarged: what Windows OCR reads best
            "-vf", "scale=1920:-2,crop=iw:ih*0.22:0:0,format=gray,negate,eq=contrast=1.5,"
                   "scale=iw*2:-2:flags=lanczos",
            "-f", "image2pipe", "-vcodec", "png", "-"]
    r = subprocess.run(cmd, capture_output=True, timeout=40, creationflags=NO_WINDOW)
    if r.returncode != 0 or not r.stdout:
        raise JobError("frame grab failed")
    return r.stdout


def _votes_at(info: VodInfo, t: float) -> list[float]:
    try:
        return [t - g for g in clock_values(ocr(top_strip(info, t)))]
    except JobError:
        return []


# ---------------------------------------------------------------- search

def find_game(info: VodInfo, lo: float, hi: float, progress: Callable[[float, str], None] = lambda f, s: None,
              cancel: threading.Event | None = None) -> dict:
    cancel = cancel or threading.Event()
    dur = info.duration or hi
    span = max(hi - lo, 60.0)
    n = 12
    times = [lo + span * (i + 0.5) / n for i in range(n)]
    progress(0.02, "Reading the game clock…")

    votes: list[tuple[float, float]] = []      # (sample time, implied game start)
    with ThreadPoolExecutor(max_workers=4) as pool:
        for k, (t, vs) in enumerate(zip(times, pool.map(lambda t: _votes_at(info, t), times))):
            if cancel.is_set():
                raise Cancelled()
            votes += [(t, v) for v in vs if -60 <= v <= dur]
            progress(0.05 + 0.45 * (k + 1) / n, "Reading the game clock…")

    if not votes:
        raise JobError("Couldn't see a League game clock in that section. "
                       "Make sure the section covers the game, with the in-game HUD visible.")
    # biggest group of agreeing votes, counting each sample time once
    best: list[tuple[float, float]] = []
    for _, v in votes:
        group = [(t, x) for t, x in votes if abs(x - v) <= TOL]
        if len({t for t, _ in group}) > len({t for t, _ in best}):
            best = group
    samples = sorted({t for t, _ in best})
    if len(samples) < 2:
        raise JobError("Found numbers on screen but no game clock that keeps time. "
                       "Try a section that's mostly the game itself.")
    start = statistics.median(x for _, x in best)

    def is_game(t: float) -> bool:
        try:
            readings = clock_values(ocr(top_strip(info, t)))
        except JobError:
            return False
        return any(abs((t - v) - start) <= TOL for g in readings for v in _minute_variants(g))

    # walk forward from the last frame that showed the clock
    last_ok = max(samples)
    t = last_ok
    misses, first_bad = 0, None
    limit = min(dur, max(hi, last_ok) + 25 * 60)
    progress(0.55, "Finding the end of the game…")
    while t + 30 <= limit:
        if cancel.is_set():
            raise Cancelled()
        t += 30
        if is_game(t):
            last_ok, misses, first_bad = t, 0, None
        else:
            misses += 1
            first_bad = first_bad or t
            if misses >= 3:
                # 90 s without the clock. Replays, death recaps or a full-screen camera
                # can hide it for a while, so look a few minutes further before calling it.
                back = next((t2 for t2 in range(int(t) + 60, int(min(limit, t + 300)) + 1, 60)
                             if not cancel.is_set() and is_game(t2)), None)
                if back is None:
                    break
                t, last_ok, misses, first_bad = back, back, 0, None
        progress(min(0.85, 0.55 + 0.3 * (t - samples[-1]) / max(1, limit - samples[-1])), "Finding the end of the game…")
    if first_bad is None:
        end = min(dur, last_ok + 10)
    else:
        a, b = last_ok, first_bad
        while b - a > 3:
            if cancel.is_set():
                raise Cancelled()
            mid = (a + b) / 2
            a, b = (mid, b) if is_game(mid) else (a, mid)
            progress(0.9, "Pinning down the last second…")
        end = min(dur, a + 10)   # a few seconds for the nexus explosion / victory screen
    progress(1.0, "Done")
    return {"game_start": start, "start": max(0.0, start - 3), "end": end,
            "readings": len(samples), "clock_at_lo": lo - start}
