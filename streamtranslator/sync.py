"""Line up two streams by wall-clock time.

Every Twitch VOD knows when its broadcast started. A moment at 1:02:30 in your
VOD is the same real-world moment as 1:02:30 + (your start - their start) in
theirs. Streamers' broadcast delays add a few seconds of slack, which is why
the UI pads the matched section a little.
"""
from __future__ import annotations

import re
import time
from urllib.parse import urlparse

from .pipeline import JobError, VodInfo, _QuietLogger, fetch_info

_LOGIN = re.compile(r"^[A-Za-z0-9_]{3,25}$")
_RESERVED = {"videos", "directory", "settings", "search", "p", "downloads", "jobs", "turbo"}


def classify(text: str) -> tuple[str, str]:
    """('vod', url) | ('channel', login) | ('url', url) for anything else yt-dlp might read."""
    s = (text or "").strip()
    if not s:
        raise JobError("Paste a link or a channel name.")
    if s.isdigit():
        return "vod", f"https://www.twitch.tv/videos/{s}"
    if _LOGIN.match(s):
        return "channel", s.lower()
    if not s.startswith("http"):
        s = "https://" + s
    u = urlparse(s)
    host = u.netloc.lower().removeprefix("www.").removeprefix("m.")
    parts = [p for p in u.path.split("/") if p]
    if host.endswith("twitch.tv"):
        if len(parts) >= 2 and parts[0] == "videos" and parts[1].isdigit():
            return "vod", s
        if parts and parts[0].lower() not in _RESERVED and _LOGIN.match(parts[0]):
            return "channel", parts[0].lower()
    return "url", s


_START_IN_THUMB = re.compile(r"_(\d{10})/+(?:thumb|storyboards)")


def list_vods(login: str, limit: int = 12) -> list[dict]:
    """Recent past broadcasts for a channel, newest first."""
    import yt_dlp
    opts = {"quiet": True, "no_warnings": True, "skip_download": True, "logger": _QuietLogger(),
            "extract_flat": True, "playlistend": limit}
    try:
        with yt_dlp.YoutubeDL(opts) as y:
            p = y.extract_info(f"https://www.twitch.tv/{login}/videos?filter=archives&sort=time", download=False)
    except Exception as e:
        msg = re.sub(r"\x1b\[[0-9;]*m", "", str(e)).replace("ERROR: ", "")
        raise JobError(f"Couldn't list {login}'s VODs.\n{msg}") from None
    out = []
    for e in p.get("entries") or []:
        thumb = e.get("thumbnail") or ""
        m = _START_IN_THUMB.search(thumb)
        out.append({
            "id": str(e.get("id", "")).lstrip("v"),
            "url": e.get("url") or f"https://www.twitch.tv/videos/{str(e.get('id', '')).lstrip('v')}",
            "title": e.get("title") or "Untitled",
            "duration": float(e.get("duration") or 0),
            "thumbnail": thumb if "404_processing" not in thumb else "",
            "start_ts": float(m.group(1)) if m else None,
            "views": e.get("view_count"),
            "channel": login,
        })
    if not out:
        raise JobError(f"{login} has no saved past broadcasts (or they're sub-only).")
    return out


def _vod_covering(vods: list[dict], moment: float) -> dict | None:
    for v in vods:
        if v["start_ts"] is None:
            continue
        if v["start_ts"] - 60 <= moment <= v["start_ts"] + v["duration"] + 60:
            return v
    return None


def match(mine: VodInfo, my_start: float, my_end: float, theirs_text: str) -> dict:
    """Find their VOD covering your section and map the times across."""
    if mine.start_ts is None:
        raise JobError("Twitch didn't say when your VOD started, so it can't be synced.")
    t0 = mine.start_ts + my_start
    t1 = mine.start_ts + my_end
    kind, value = classify(theirs_text)
    if kind == "channel":
        vods = list_vods(value, limit=20)
        hit = _vod_covering(vods, (t0 + t1) / 2) or _vod_covering(vods, t0)
        if not hit:
            when = time.strftime("%b %d %H:%M", time.localtime(t0))
            raise JobError(f"{value} wasn't live at {when} (checked their last {len(vods)} VODs).")
        theirs = fetch_info(hit["url"])
    elif kind == "vod":
        theirs = fetch_info(value)
    elif not theirs_text.strip().lower().startswith("http"):
        raise JobError(f"Couldn't find a Twitch channel called \"{theirs_text.strip()}\".")
    else:
        raise JobError("For syncing, give their Twitch channel name or a Twitch VOD link.")
    if theirs.start_ts is None:
        raise JobError("Twitch didn't say when their VOD started, so it can't be synced.")

    offset = mine.start_ts - theirs.start_ts            # add to my time to get theirs
    start, end = my_start + offset, my_end + offset
    if end < 0 or start > theirs.duration:
        raise JobError("Their VOD doesn't cover that moment. They may not have been live yet, "
                       "or the VOD was trimmed.")
    clipped = start < 0 or end > theirs.duration
    return {
        "theirs": theirs,
        "start": max(0.0, start),
        "end": min(theirs.duration, end),
        "offset": offset,
        "clipped": clipped,
    }
