"""Command-line version:  python -m streamtranslator.cli <vod url> --start 1:02:30 --end 1:35:00"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

from .pipeline import Callbacks, Cancelled, JobError, JobParams, POSITIONS, FONT_SIZES, run_job
from .timefmt import format_ts, parse_ts


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="streamtranslator")
    ap.add_argument("url")
    ap.add_argument("--start", required=True, type=parse_ts)
    ap.add_argument("--end", required=True, type=parse_ts)
    ap.add_argument("--out", default=str(Path.home() / "Videos" / "StreamTranslator"))
    ap.add_argument("--quality", default="720p", choices=["Source", "720p", "480p"])
    ap.add_argument("--model", default="large-v3")
    ap.add_argument("--language", default=None, help="e.g. ko, es (default: auto-detect)")
    ap.add_argument("--original", action="store_true", help="also save original-language subtitles")
    ap.add_argument("--no-burn", action="store_true")
    ap.add_argument("--position", default="Above game HUD", choices=list(POSITIONS))
    ap.add_argument("--size", default="Medium", choices=list(FONT_SIZES))
    a = ap.parse_args(argv)
    from . import updater
    updater.activate()

    for s in (sys.stdout, sys.stderr):
        try:
            s.reconfigure(encoding="utf-8", errors="replace")
        except Exception:
            pass

    last = {"pct": -1}

    def prog(f: float):
        pct = int(f * 100) if f >= 0 else -1
        if pct != last["pct"] and pct % 10 == 0:
            last["pct"] = pct
            print(f"    {pct}%", flush=True)

    def stage(i, n, label):
        last["pct"] = -1
        print(f"[{i}/{n}] {label}", flush=True)

    cb = Callbacks(stage=stage, progress=prog, log=lambda s: print("  " + s, flush=True),
                   segment=lambda seg, kind: print(f"    {format_ts(seg.start):>6} [{kind}] {seg.text}", flush=True))
    p = JobParams(url=a.url, start=a.start, end=a.end, out_dir=Path(a.out), quality=a.quality,
                  model=a.model, language=a.language, want_original=a.original, burn=not a.no_burn,
                  position=a.position, font_size=a.size)
    try:
        r = run_job(p, cb)
    except (JobError, Cancelled) as e:
        print(f"Failed: {e}", file=sys.stderr)
        return 1
    print(f"\nDone in {r.seconds:.0f}s. Language: {r.language} ({r.probability:.0%}), {r.segments} lines")
    print(f"  Folder:     {r.folder}\n  Subtitles:  {r.srt.name}\n  Transcript: {r.transcript.name}")
    if r.subbed:
        print(f"  Video:      {r.subbed.name}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
