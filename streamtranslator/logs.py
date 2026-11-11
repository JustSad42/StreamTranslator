"""Logging, crash capture and "Report a problem".

- logs/app.log: what the app did (job steps, errors with tracebacks), with timestamps.
  Kept small: 2 MB, plus 2 older files.
- logs/crash.log: hard crashes (a native library taking the whole app down), written by faulthandler.
- running.flag exists while the app runs; if it's still there at the next start, the app didn't
  close normally, and the interface offers to report it.
"""
from __future__ import annotations

import faulthandler
import logging
import logging.handlers
import os
import platform
import re
import sys
import threading
import time
import urllib.parse
from pathlib import Path

from . import __version__

LOG_DIR = Path(os.environ.get("LOCALAPPDATA", Path.home())) / "StreamTranslator" / "logs"
APP_LOG = LOG_DIR / "app.log"
CRASH_LOG = LOG_DIR / "crash.log"
FLAG = LOG_DIR / "running.flag"
ISSUES = "https://github.com/JustSad42/StreamTranslator/issues/new"

log = logging.getLogger("streamtranslator")
_crash_file = None
previous_crash: dict | None = None


class _ToLog:
    """Stands in for stdout/stderr in the windowed app: library prints end up in the log."""
    def __init__(self, level):
        self.level = level
        self._buf = ""

    def write(self, s):
        self._buf += s
        while "\n" in self._buf or "\r" in self._buf:
            line, _, self._buf = re.split(r"([\r\n])", self._buf, maxsplit=1)
            line = line.rstrip()
            if line and "%|" not in line:          # skip download progress bars
                log.log(self.level, line)
        return len(s)

    def flush(self):
        pass

    def isatty(self):
        return False


def setup(windowed: bool = False):
    global _crash_file, previous_crash
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    h = logging.handlers.RotatingFileHandler(APP_LOG, maxBytes=2_000_000, backupCount=2, encoding="utf-8")
    h.setFormatter(logging.Formatter("%(asctime)s %(levelname)-7s %(threadName)s: %(message)s", "%Y-%m-%d %H:%M:%S"))
    root = logging.getLogger()
    root.addHandler(h)
    root.setLevel(logging.INFO)
    if windowed:                                          # windowed app: no console
        sys.stdout = _ToLog(logging.INFO)
        sys.stderr = _ToLog(logging.WARNING)
    # native crashes: Python can't catch them, faulthandler writes the stack of every thread
    _crash_file = open(CRASH_LOG, "a", encoding="utf-8", buffering=1)
    faulthandler.enable(_crash_file, all_threads=True)

    def hook(exc_type, exc, tb):
        log.critical("Unhandled error", exc_info=(exc_type, exc, tb))
    sys.excepthook = hook
    threading.excepthook = lambda a: log.error(f"Error in thread {a.thread.name if a.thread else '?'}",
                                               exc_info=(a.exc_type, a.exc_value, a.exc_traceback))

    if FLAG.exists():
        try:
            started = FLAG.read_text(encoding="utf-8").strip()
        except OSError:
            started = ""
        previous_crash = {"started": started, "native": _recent_native_crash()}
        log.warning(f"The previous session didn't close normally (started {started}).")
    FLAG.write_text(f"{__version__} {time.strftime('%Y-%m-%d %H:%M:%S')}", encoding="utf-8")
    log.info(f"StreamTranslator {__version__} started on Windows {platform.version()}")


def clean_exit():
    log.info("Closed normally.")
    try:
        FLAG.unlink(missing_ok=True)
    except OSError:
        pass


def _recent_native_crash() -> bool:
    try:
        return CRASH_LOG.stat().st_size > 0 and time.time() - CRASH_LOG.stat().st_mtime < 7 * 86400 \
            and "fatal" in _tail(CRASH_LOG, 40).lower()
    except OSError:
        return False


def _tail(path: Path, lines: int) -> str:
    try:
        with open(path, "rb") as f:
            f.seek(0, 2)
            f.seek(max(0, f.tell() - 200_000))
            return "\n".join(f.read().decode("utf-8", "replace").splitlines()[-lines:])
    except OSError:
        return ""


def redact(text: str) -> str:
    """Hide the Windows user name in paths."""
    home = str(Path.home())
    text = text.replace(home, "~").replace(home.replace("\\", "/"), "~")
    return re.sub(r"(?i)[A-Z]:\\Users\\[^\\\s]+", r"~", text)


def report_url(what: str = "", extra: dict | None = None) -> str:
    """A pre-filled GitHub issue. Nothing is sent: it opens in the browser for the user to read and submit."""
    gpu = (extra or {}).pop("gpu", "") if extra else ""
    info = [f"- Version: {__version__}", f"- Windows: {platform.version()}", f"- GPU: {gpu or 'unknown'}"]
    for k, v in (extra or {}).items():
        info.append(f"- {k}: {v}")
    crash = _tail(CRASH_LOG, 25) if _recent_native_crash() else ""
    def body(n_lines: int) -> str:
        parts = ["**What happened?**", what or "(describe what you were doing)", "", "**Details**", *info, "",
                 "<details><summary>Recent log</summary>", "", "```", redact(_tail(APP_LOG, n_lines)), "```", "</details>"]
        if crash:
            parts += ["", "<details><summary>Crash log</summary>", "", "```", redact(crash), "```", "</details>"]
        return "\n".join(parts)
    title = f"[{__version__}] " + ((what or "Problem").splitlines()[0][:80])
    # browsers and GitHub cut very long links: shrink the log until it fits
    for n in (80, 50, 30, 15, 5):
        url = ISSUES + "?" + urllib.parse.urlencode({"title": title, "body": body(n)})
        if len(url) < 7500:
            return url
    return ISSUES + "?" + urllib.parse.urlencode({"title": title, "body": body(0)[:3000]})
