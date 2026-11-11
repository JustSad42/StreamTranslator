"""Make the pip-installed NVIDIA libraries (cuBLAS, cuDNN) findable on Windows.

faster-whisper (CTranslate2) loads these DLLs by name. They ship inside
site-packages/nvidia/*/bin, which Windows does not search by default.
Must run before faster_whisper / ctranslate2 is imported.
"""
from __future__ import annotations

import os
import site
import sys
from pathlib import Path

_done = False


def setup() -> None:
    global _done
    if _done:
        return
    _done = True
    roots = set(site.getsitepackages())
    roots.add(str(Path(sys.prefix) / "Lib" / "site-packages"))
    if getattr(sys, "frozen", False):     # packaged app: libraries live next to the exe
        roots.add(str(getattr(sys, "_MEIPASS", Path(sys.executable).parent)))
    bins: list[str] = []
    for root in roots:
        nv = Path(root) / "nvidia"
        if nv.is_dir():
            bins += [str(p) for p in nv.glob("*/bin") if p.is_dir()]
    for b in bins:
        try:
            os.add_dll_directory(b)
        except (OSError, AttributeError):
            pass
    if bins:
        os.environ["PATH"] = os.pathsep.join(bins + [os.environ.get("PATH", "")])
