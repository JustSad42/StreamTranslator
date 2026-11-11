"""Library of past translations (clips and live sessions), with transcript search."""
from __future__ import annotations

import json
import re
import threading
import time
import uuid
from pathlib import Path

from .glossary import DATA_DIR

FILE = DATA_DIR / "history.json"
_lock = threading.Lock()


def _load() -> list[dict]:
    try:
        return json.loads(FILE.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return []


def _save(items: list[dict]):
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    FILE.write_text(json.dumps(items[:500], indent=1, ensure_ascii=False), encoding="utf-8")


def add(entry: dict) -> dict:
    entry = {"id": uuid.uuid4().hex[:12], "created": time.time(), **entry}
    with _lock:
        items = [e for e in _load() if e.get("folder") != entry.get("folder") or entry.get("type") == "live"]
        items.insert(0, entry)
        _save(items)
    return entry


def update(entry_id: str, **changes) -> dict | None:
    with _lock:
        items = _load()
        for e in items:
            if e["id"] == entry_id:
                e.update(changes)
                _save(items)
                return e
    return None


def remove(entry_id: str):
    with _lock:
        _save([e for e in _load() if e["id"] != entry_id])


def listing() -> list[dict]:
    items = _load()
    for e in items:
        e["exists"] = bool(e.get("folder")) and Path(e["folder"]).exists() if e.get("type") == "clip" \
            else bool(e.get("transcript")) and Path(e["transcript"]).exists()
    return items


def search(q: str, limit_hits: int = 3) -> dict[str, dict]:
    """Search titles, channels and transcript text. Returns {id: {hits, snippets}}."""
    q = q.strip().lower()
    out: dict[str, dict] = {}
    if not q:
        return out
    for e in _load():
        hits, snippets = 0, []
        meta = f"{e.get('title', '')} {e.get('channel', '')}".lower()
        if q in meta:
            hits += 1
        tx = e.get("transcript")
        if tx and Path(tx).exists():
            for line in Path(tx).read_text(encoding="utf-8", errors="replace").splitlines():
                if q in line.lower():
                    hits += 1
                    if len(snippets) < limit_hits:
                        m = re.match(r"\[(?:VOD\s+)?([\d:]+)", line.strip())
                        text = re.sub(r"^\[[^\]]*\]\s*", "", line.strip())
                        snippets.append({"time": m.group(1) if m else "", "text": text[:200]})
        if hits:
            out[e["id"]] = {"hits": hits, "snippets": snippets}
    return out
