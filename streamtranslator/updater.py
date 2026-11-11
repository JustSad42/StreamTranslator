"""Keep yt-dlp current without reinstalling the app.

Twitch changes its site often, and yt-dlp needs updating when it does. On each
start the app checks PyPI; a newer yt-dlp wheel is downloaded, its SHA-256 is
verified against PyPI's, and it's unpacked to %LOCALAPPDATA%/StreamTranslator/pylib.
`activate()` (called before anything imports yt_dlp) puts the newest copy first
on sys.path. yt-dlp is pure Python, so this also works inside the packaged .exe.
"""
from __future__ import annotations

import hashlib
import importlib.util
import io
import json
import os
import re
import shutil
import sys
import urllib.request
import zipfile
from pathlib import Path

PYLIB = Path(os.environ.get("LOCALAPPDATA", Path.home())) / "StreamTranslator" / "pylib"
_active: str | None = None


def _vt(v: str) -> tuple[int, ...]:
    return tuple(int(x) for x in re.findall(r"\d+", v or "0"))


def _read_version(pkg_parent: Path) -> str | None:
    try:
        txt = (pkg_parent / "yt_dlp" / "version.py").read_text(encoding="utf-8")
        m = re.search(r"__version__\s*=\s*['\"]([^'\"]+)", txt)
        return m.group(1) if m else None
    except OSError:
        return None


def _bundled() -> tuple[str | None, Path | None]:
    if getattr(sys, "frozen", False):            # packaged app ships yt-dlp as plain files
        base = Path(getattr(sys, "_MEIPASS", Path(sys.executable).parent)) / "ytdlp_bundled"
        if base.is_dir() and str(base) not in sys.path:
            sys.path.append(str(base))
    spec = importlib.util.find_spec("yt_dlp")
    if not spec or not spec.origin:
        return None, None
    parent = Path(spec.origin).parent.parent
    return _read_version(parent), parent


def activate() -> str | None:
    """Use the newest downloaded yt-dlp if it beats the bundled one. Call before importing yt_dlp."""
    global _active
    if "yt_dlp" in sys.modules:
        return _active or getattr(sys.modules["yt_dlp"].version, "__version__", None)
    bundled, _ = _bundled()
    best_dir, best_ver = None, bundled
    if PYLIB.is_dir():
        for d in PYLIB.glob("yt-dlp-*"):
            v = _read_version(d)
            if v and _vt(v) > _vt(best_ver or "0"):
                best_dir, best_ver = d, v
    if best_dir:
        sys.path.insert(0, str(best_dir))
        importlib.invalidate_caches()
    _active = best_ver
    return _active


def active_version() -> str | None:
    return _active


def check_and_update(timeout: float = 15) -> dict:
    """Download a newer yt-dlp if PyPI has one. Returns {current, latest, updated}."""
    req = urllib.request.Request("https://pypi.org/pypi/yt-dlp/json", headers={"User-Agent": "StreamTranslator"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        data = json.loads(r.read().decode("utf-8"))
    latest = data["info"]["version"]
    current = _active or _bundled()[0]
    if _vt(latest) <= _vt(current or "0"):
        return {"current": current, "latest": latest, "updated": False}
    wheel = next((u for u in data["urls"] if u["packagetype"] == "bdist_wheel" and u["filename"].endswith("none-any.whl")), None)
    if not wheel:
        return {"current": current, "latest": latest, "updated": False, "error": "no wheel published"}
    req = urllib.request.Request(wheel["url"], headers={"User-Agent": "StreamTranslator"})
    with urllib.request.urlopen(req, timeout=60) as r:
        blob = r.read()
    if hashlib.sha256(blob).hexdigest() != wheel["digests"]["sha256"]:
        raise RuntimeError("yt-dlp download failed its checksum; not installed.")
    target = PYLIB / f"yt-dlp-{latest}"
    tmp = PYLIB / f".tmp-{latest}"
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir(parents=True)
    with zipfile.ZipFile(io.BytesIO(blob)) as z:
        for m in z.namelist():
            if m.startswith("yt_dlp/") and ".." not in m:
                z.extract(m, tmp)
    if not _read_version(tmp):
        shutil.rmtree(tmp, ignore_errors=True)
        raise RuntimeError("yt-dlp download looked incomplete; not installed.")
    shutil.rmtree(target, ignore_errors=True)
    tmp.rename(target)
    # keep the newest two copies
    olds = sorted((d for d in PYLIB.glob("yt-dlp-*") if d != target), key=lambda d: _vt(d.name))
    for d in olds[:-1]:
        shutil.rmtree(d, ignore_errors=True)
    return {"current": current, "latest": latest, "updated": True}
