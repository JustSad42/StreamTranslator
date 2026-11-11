"""Update the app itself from its GitHub releases.

`check()` asks GitHub for the latest release. `download()` fetches its installer and checks
the SHA-256 GitHub publishes for it. `install()` waits for this app to close, then runs the
installer silently; the installer starts the app again when it's done.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
import urllib.request
from pathlib import Path

from . import __version__

REPO = "JustSad42/StreamTranslator"
API = f"https://api.github.com/repos/{REPO}/releases/latest"
PAGE = f"https://github.com/{REPO}/releases/latest"
DIR = Path(os.environ.get("LOCALAPPDATA", Path.home())) / "StreamTranslator" / "updates"
HEADERS = {"User-Agent": "StreamTranslator", "Accept": "application/vnd.github+json"}


def _vt(v: str) -> tuple[int, ...]:
    return tuple(int(x) for x in re.findall(r"\d+", v or "0")[:3])


def is_newer(latest: str, current: str = __version__) -> bool:
    return _vt(latest) > _vt(current)


def _release(url: str, timeout: float) -> dict:
    req = urllib.request.Request(url, headers=HEADERS)
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode("utf-8"))


def release_info(version: str, timeout: float = 15) -> dict:
    """Same shape as check(), for one specific version (used to go back to the previous one)."""
    return _parse(_release(f"https://api.github.com/repos/{REPO}/releases/tags/v{version.lstrip('vV')}", timeout))


def save_notes(version: str, notes: str):
    DIR.mkdir(parents=True, exist_ok=True)
    (DIR / f"notes-{version}.md").write_text(notes or "", encoding="utf-8")


def notes_for(version: str) -> str:
    """Release notes for `version`: saved when the update was downloaded, else asked from GitHub."""
    try:
        return (DIR / f"notes-{version}.md").read_text(encoding="utf-8")
    except OSError:
        pass
    try:
        return release_info(version, timeout=8).get("notes", "")
    except Exception:
        return ""


def check(timeout: float = 15) -> dict:
    """{current, latest, newer, notes, url, asset: {name, url, size, sha256}}"""
    return _parse(_release(API, timeout))


def _parse(rel: dict) -> dict:
    latest = rel.get("tag_name", "").lstrip("vV")
    asset = next((a for a in rel.get("assets", [])
                  if a["name"].lower().startswith("streamtranslator-setup") and a["name"].lower().endswith(".exe")), None)
    info = {"current": __version__, "latest": latest, "notes": (rel.get("body") or "").strip(),
            "url": rel.get("html_url") or PAGE, "asset": None,
            "newer": bool(asset) and is_newer(latest)}
    if asset:
        digest = asset.get("digest") or ""
        info["asset"] = {"name": asset["name"], "url": asset["browser_download_url"], "size": asset.get("size", 0),
                         "sha256": digest.split(":", 1)[1].lower() if digest.startswith("sha256:") else ""}
    return info


def download(asset: dict, progress=None, cancel=None) -> Path:
    """Download the installer to DIR, verify size and checksum, return its path."""
    DIR.mkdir(parents=True, exist_ok=True)
    name = Path(asset["name"]).name
    target, part = DIR / name, DIR / (name + ".part")
    for old in DIR.glob("StreamTranslator-Setup-*"):
        if old != target:
            old.unlink(missing_ok=True)
    size, h, got = asset.get("size") or 0, hashlib.sha256(), 0
    req = urllib.request.Request(asset["url"], headers={"User-Agent": "StreamTranslator"})
    with urllib.request.urlopen(req, timeout=60) as r, open(part, "wb") as f:
        while chunk := r.read(1 << 20):
            if cancel and cancel.is_set():
                f.close()
                part.unlink(missing_ok=True)
                raise RuntimeError("Update cancelled")
            f.write(chunk)
            h.update(chunk)
            got += len(chunk)
            if progress and size:
                progress(got / size)
    if size and got != size:
        part.unlink(missing_ok=True)
        raise RuntimeError("The update download was incomplete. Try again.")
    if asset.get("sha256") and h.hexdigest() != asset["sha256"]:
        part.unlink(missing_ok=True)
        raise RuntimeError("The update failed its checksum, so it wasn't installed.")
    target.unlink(missing_ok=True)
    part.rename(target)
    return target


def install(installer: Path):
    """Run the installer once this process has exited. The caller quits the app right after."""
    if not getattr(sys, "frozen", False):
        raise RuntimeError("Updates install into the installed app. You're running from the source folder.")
    q = lambda p: str(p).replace("'", "''")
    log = DIR / "install.log"
    ps = (f"Wait-Process -Id {os.getpid()} -Timeout 90 -ErrorAction SilentlyContinue; "
          f"Start-Process -FilePath '{q(installer)}' "
          f"-ArgumentList '/SILENT','/SUPPRESSMSGBOXES','/NORESTART','/LOG=\"{q(log)}\"'")
    run_helper(ps)


def run_helper(ps: str) -> subprocess.Popen:
    """Run a PowerShell command in the background that outlives this app.

    It needs CREATE_NO_WINDOW (a hidden console of its own). 2.2.0 and 2.3.0 used DETACHED_PROCESS,
    which leaves PowerShell with no console at all, and it then quietly does nothing: the update was
    downloaded but the installer never started."""
    args = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-Command", ps]
    no_window, new_group, breakaway = 0x08000000, 0x00000200, 0x01000000
    try:   # also leave any job object the app runs in, so closing the app can't take the helper with it
        return subprocess.Popen(args, creationflags=no_window | new_group | breakaway, close_fds=True)
    except OSError:
        return subprocess.Popen(args, creationflags=no_window | new_group, close_fds=True)
