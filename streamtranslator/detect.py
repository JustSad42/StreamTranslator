"""Read who's in the game off the video, to give the glossary the right names.

- Champions: their portraits (broadcast scoreboard, your HUD, ally frames) are matched
  against Riot's official champion icons from Data Dragon.
- Players: on-screen nameplates like "T1 Faker" / "BLG Knight" are read with Windows OCR.

Both feed Whisper's prompt and the clean-up step for that one job. Nothing is saved to
your glossary, so a wrong guess can't stick.
"""
from __future__ import annotations

import os
import re
import subprocess
import threading
import urllib.request
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np

from .glossary import COMMON, DDRAGON, Glossary, _norm
from .pipeline import NO_WINDOW, JobError, VodInfo, ffmpeg_exe

ICON_DIR = Path(os.environ.get("LOCALAPPDATA", Path.home())) / "StreamTranslator" / "icons"
SIZES = (18, 22, 26, 31, 37, 44, 52)          # portrait sizes searched, in a 1280-wide frame
THRESHOLD = 0.80
_NAMEPLATE = re.compile(r"\b([A-Z][A-Z0-9]{1,4})\s+([A-Za-z0-9][A-Za-z0-9_.\-]{1,15})\b")
_icons_cache: dict | None = None
_icons_lock = threading.Lock()


def _frame(info: VodInfo, t: float, width: int = 1920) -> bytes:
    url = info.streams.get("Source") or info.streams.get("720p") or info.preview_stream
    cmd = [ffmpeg_exe(), "-hide_banner", "-nostdin", "-loglevel", "error"]
    if info.user_agent:
        cmd += ["-user_agent", info.user_agent]
    cmd += ["-ss", f"{max(0.0, t):.2f}", "-i", url, "-frames:v", "1", "-vf", f"scale={width}:-2",
            "-f", "image2pipe", "-vcodec", "png", "-"]
    r = subprocess.run(cmd, capture_output=True, timeout=40, creationflags=NO_WINDOW)
    if r.returncode != 0 or not r.stdout:
        raise JobError("frame grab failed")
    return r.stdout


def _icons() -> dict[str, list]:
    """{champion name: [template images at each size]} from Data Dragon (downloaded once)."""
    global _icons_cache
    import cv2
    with _icons_lock:
        if _icons_cache is not None:
            return _icons_cache
        gl = Glossary.get()
        champs = [c for c in gl.champions if c.get("id")]
        if not champs or not gl.version:
            raise JobError("Champion icons need the champion list from Riot (connect once and restart).")
        folder = ICON_DIR / gl.version
        folder.mkdir(parents=True, exist_ok=True)

        def fetch(c):
            p = folder / f"{c['id']}.png"
            if not p.exists():
                req = urllib.request.Request(f"{DDRAGON}/cdn/{gl.version}/img/champion/{c['id']}.png",
                                             headers={"User-Agent": "StreamTranslator"})
                with urllib.request.urlopen(req, timeout=15) as r:
                    p.write_bytes(r.read())
            img = cv2.imdecode(np.frombuffer(p.read_bytes(), np.uint8), cv2.IMREAD_COLOR)
            h, w = img.shape[:2]
            m = int(min(h, w) * 0.13)                     # drop the border: HUDs frame portraits differently
            core = img[m:h - m, m:w - m]
            return c["name"], [cv2.resize(core, (s, s), interpolation=cv2.INTER_AREA) for s in SIZES]

        with ThreadPoolExecutor(max_workers=16) as ex:
            _icons_cache = dict(ex.map(fetch, champs))
        return _icons_cache


def champions_in(frames: list[np.ndarray]) -> list[tuple[str, float]]:
    """Champions whose portrait matches Riot's icon well in any frame. Roughly 8 of 10 right on
    broadcast layouts; a wrong extra name only costs a stray word in Whisper's prompt."""
    import cv2
    cv2.setNumThreads(1)
    icons = _icons()

    def best(args):
        fi, (name, temps) = args
        fr = frames[fi]
        top = (0.0, 0, 0, 0)
        for t in temps:
            if t.shape[0] < fr.shape[0] and t.shape[1] < fr.shape[1]:
                _, v, _, loc = cv2.minMaxLoc(cv2.matchTemplate(fr, t, cv2.TM_CCOEFF_NORMED))
                if v > top[0]:
                    top = (v, loc[0], loc[1], t.shape[0])
        return fi, name, top

    jobs = [(fi, item) for fi in range(len(frames)) for item in icons.items()]
    with ThreadPoolExecutor(max_workers=max(4, (os.cpu_count() or 8) - 2)) as ex:
        results = list(ex.map(best, jobs))
    # best score per champion over all frames (tested best: 16 of 20 right on two streams)
    best_score: dict[str, float] = {}
    for _, name, (v, *_rest) in results:
        best_score[name] = max(best_score.get(name, 0.0), v)
    hits = sorted(((n, v) for n, v in best_score.items() if v >= THRESHOLD), key=lambda h: -h[1])
    return hits[:10]


_HUD_WORDS = {"patch", "live", "replay", "semifinals", "finals", "quarterfinals", "baron", "dragon", "elder",
              "herald", "gold", "kills", "towers", "victory", "defeat", "worlds", "msi", "nearsighted", "game",
              "lck", "lpl", "lec", "lcs", "ljl", "pcs", "vcs", "cblol", "lta", "blg", "geng", "hle", "jdg", "tes",
              "kc", "fnc", "g2", "t1", "kt", "dk", "drx", "fox", "ns", "bro", "you", "the", "and", "for", "all",
              "team", "red", "blue", "level", "shop", "ping", "fps", "stats", "kda", "subs", "follow", "donate"}


def players_in(ocr_lines: list[list[str]]) -> list[str]:
    """Player names: "T1 Faker"-style nameplates, plus short names that sit on screen
    (scoreboard) in most frames."""
    champ_keys = {_norm(c["name"]) for c in Glossary.get().champions}

    def ok(name: str) -> bool:
        key = name.lower()
        return (3 <= len(name) <= 14 and not name.isdigit() and sum(c.isdigit() for c in name) <= 2
                and key not in COMMON and key not in _HUD_WORDS and _norm(name) not in champ_keys)

    strong: Counter = Counter()
    seen_words: Counter = Counter()
    for lines in ocr_lines:
        here_strong, here_words = set(), set()
        for line in lines:
            line = re.sub(r"\b(T[lI1|])[\s_]", "T1 ", line)              # OCR reads "T1" as "Tl"
            line = re.sub(r"\b([A-Z][A-Z0-9]{1,4})_", r"\1 ", line)       # "BLG_Knight"
            for team, name in _NAMEPLATE.findall(line):
                if ok(name) and team not in ("LPL", "LCK", "LEC", "LCS", "GG", "MVP", "KDA", "CS"):
                    here_strong.add(name[0].upper() + name[1:])
            for w in re.findall(r"[A-Za-z][A-Za-z0-9]{2,13}", line):
                if ok(w) and w.isalpha() and (w[0].isupper() and (w[1:].islower() or (w.isupper() and len(w) <= 6))):
                    here_words.add(w.capitalize() if w.isupper() else w)
        strong.update(here_strong)
        seen_words.update(here_words)
    n = len(ocr_lines)
    names = [w for w, c in strong.most_common() if c >= (2 if n > 2 else 1)]
    names += [w for w, c in seen_words.most_common() if c >= max(2, n - 1) and w not in names]
    return names[:12]


def game_details(info: VodInfo, start: float, end: float, cancel: threading.Event | None = None,
                 samples: int = 4) -> dict:
    import cv2
    from .snap import ocr
    span = max(end - start, 1.0)
    times = [start + span * (i + 1) / (samples + 1) for i in range(samples)]
    with ThreadPoolExecutor(max_workers=4) as ex:
        pngs = list(ex.map(lambda t: _try(_frame, info, t), times))
    pngs = [p for p in pngs if p]
    if cancel is not None and cancel.is_set():
        return {}
    frames = []
    for p in pngs:
        img = cv2.imdecode(np.frombuffer(p, np.uint8), cv2.IMREAD_COLOR)
        frames.append(cv2.resize(img, (1280, int(img.shape[0] * 1280 / img.shape[1])), interpolation=cv2.INTER_AREA))
    champs = champions_in(frames) if frames else []
    lines = [_try(ocr, p) or [] for p in pngs]
    return {"champions": [c for c, _ in champs], "champion_scores": champs, "players": players_in(lines)}


def _try(fn, *a):
    try:
        return fn(*a)
    except Exception:
        return None
