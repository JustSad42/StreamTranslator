"""Timestamp parsing and formatting."""
from __future__ import annotations

import re

_HMS = re.compile(r"^\s*(?:(\d+)h)?\s*(?:(\d+)m)?\s*(?:(\d+(?:\.\d+)?)s)?\s*$", re.I)


def parse_ts(text: str) -> float:
    """Accepts '1:02:30', '62:30', '3750', '1h2m30s', '45m'. Raises ValueError."""
    s = (text or "").strip()
    if not s:
        raise ValueError("empty timestamp")
    if ":" in s:
        parts = s.split(":")
        if len(parts) > 3 or any(p.strip() == "" for p in parts):
            raise ValueError(f"bad timestamp: {text!r}")
        total = 0.0
        for p in parts:
            total = total * 60 + float(p)
        return total
    try:
        return float(s)
    except ValueError:
        pass
    m = _HMS.match(s)
    if m and any(m.groups()):
        h, mi, sec = m.groups()
        return int(h or 0) * 3600 + int(mi or 0) * 60 + float(sec or 0)
    raise ValueError(f"bad timestamp: {text!r}")


def format_ts(seconds: float) -> str:
    """3750 -> '1:02:30', 95 -> '1:35'."""
    s = int(round(max(0.0, seconds)))
    h, rem = divmod(s, 3600)
    m, sec = divmod(rem, 60)
    return f"{h}:{m:02d}:{sec:02d}" if h else f"{m}:{sec:02d}"


def format_hms_tag(seconds: float) -> str:
    """3750 -> '1h02m30s' (safe for file names and Twitch ?t= links)."""
    s = int(max(0.0, seconds))
    h, rem = divmod(s, 3600)
    m, sec = divmod(rem, 60)
    return f"{h}h{m:02d}m{sec:02d}s"


def srt_ts(seconds: float) -> str:
    ms = int(round(max(0.0, seconds) * 1000))
    h, rem = divmod(ms, 3_600_000)
    m, rem = divmod(rem, 60_000)
    s, ms = divmod(rem, 1000)
    return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"
