"""Make NVIDIA's GPU libraries (cuBLAS, cuDNN) findable on Windows.

faster-whisper (CTranslate2) loads these DLLs by name. They can come from:
- pip packages in the dev environment (site-packages/nvidia/*/bin),
- the copy StreamTranslator downloaded (%LOCALAPPDATA%/StreamTranslator/cuda/*/bin),
- an existing install the setup screen found (CUDA Toolkit, another Whisper tool).
Must run before faster_whisper / ctranslate2 is imported.
"""
from __future__ import annotations

import json
import os
import site
import sys
from pathlib import Path

_done = False
BASE = Path(os.environ.get("LOCALAPPDATA", Path.home())) / "StreamTranslator"


def bundled_dirs() -> list[str]:
    roots = set(site.getsitepackages())
    roots.add(str(Path(sys.prefix) / "Lib" / "site-packages"))
    if getattr(sys, "frozen", False):
        roots.add(str(getattr(sys, "_MEIPASS", Path(sys.executable).parent)))
    out = []
    for root in roots:
        nv = Path(root) / "nvidia"
        if nv.is_dir():
            out += [str(p) for p in nv.glob("*/bin") if p.is_dir()]
    return out


def search_dirs() -> list[str]:
    dirs = bundled_dirs()
    dirs += [str(p) for p in (BASE / "cuda").glob("*/bin") if p.is_dir()]
    try:
        saved = json.loads((BASE / "gpu_libs.json").read_text(encoding="utf-8"))["dir"]
        if Path(saved).is_dir():
            dirs.append(saved)
    except (OSError, ValueError, KeyError):
        pass
    return dirs


def setup() -> None:
    global _done
    if _done:
        return
    _done = True
    dirs = search_dirs()
    for b in dirs:
        try:
            os.add_dll_directory(b)
        except (OSError, AttributeError):
            pass
    if dirs:
        os.environ["PATH"] = os.pathsep.join(dirs + [os.environ.get("PATH", "")])


def refresh() -> None:
    """After the setup screen downloads or finds libraries, make them visible right away."""
    global _done
    _done = False
    setup()
