"""Optional downloads the installer leaves out, and reuse of what's already on the PC.

The installer is small because it doesn't carry NVIDIA's GPU libraries (cuBLAS + cuDNN,
~1.3 GB) or any models. On first run the setup screen:

- finds GPU libraries that are already installed (CUDA Toolkit 12 + cuDNN 9, or the copy
  inside another faster-whisper tool such as Faster-Whisper-XXL / Subtitle Edit), or
  downloads NVIDIA's official packages from PyPI (checksum-verified);
- downloads Whisper / translator models into the shared Hugging Face cache (any other app
  that uses faster-whisper shares them), or uses a model folder you point it at.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import threading
import urllib.request
import zipfile
from pathlib import Path
from typing import Callable

BASE = Path(os.environ.get("LOCALAPPDATA", Path.home())) / "StreamTranslator"
CUDA_DIR = BASE / "cuda"
NO_WINDOW = subprocess.CREATE_NO_WINDOW if sys.platform == "win32" else 0

# exact versions this app is tested with
GPU_PACKAGES = [("nvidia-cublas-cu12", "12.9.2.10", ["cublas64_12.dll", "cublasLt64_12.dll"]),
                ("nvidia-cudnn-cu12", "9.28.1.4", ["cudnn64_9.dll", "cudnn_ops64_9.dll", "cudnn_cnn64_9.dll"])]
NEEDED = ["cublas64_12.dll", "cublasLt64_12.dll", "cudnn64_9.dll", "cudnn_ops64_9.dll", "cudnn_cnn64_9.dll"]

WHISPER_MODELS = {
    "large-v3": ("Systran/faster-whisper-large-v3", 3.1, "Best"),
    "large-v3-turbo": ("mobiuslabsgmbh/faster-whisper-large-v3-turbo", 1.6, "Turbo"),
    "medium": ("Systran/faster-whisper-medium", 1.5, "Fast"),
    "small": ("Systran/faster-whisper-small", 0.5, "Fastest"),
}


def _has_all(folder: Path) -> bool:
    return all((folder / n).exists() for n in NEEDED)


def _candidate_dirs() -> list[tuple[Path, str]]:
    """Folders that might already hold cuBLAS 12 + cuDNN 9."""
    out: list[tuple[Path, str]] = []
    cuda_path = os.environ.get("CUDA_PATH")
    if cuda_path:
        out.append((Path(cuda_path) / "bin", "CUDA Toolkit"))
    for p in os.environ.get("PATH", "").split(os.pathsep):
        if p and "cudnn" in p.lower():
            out.append((Path(p), "cuDNN on PATH"))
    home = Path.home()
    for root, label in ((home / "Desktop", "Faster-Whisper-XXL"), (home / "Downloads", "Faster-Whisper-XXL"),
                        (Path("C:/"), "Faster-Whisper-XXL"), (Path(os.environ.get("ProgramFiles", "C:/Program Files")), "Faster-Whisper-XXL")):
        for d in list(root.glob("Faster-Whisper-XXL*"))[:3] + list(root.glob("*/Faster-Whisper-XXL*"))[:3]:
            out += [(d, label), (d / "_xxl_data", label)]
    se = Path(os.environ.get("APPDATA", "")) / "Subtitle Edit" / "Whisper"
    if se.is_dir():
        out += [(d, "Subtitle Edit") for d in se.glob("*") if d.is_dir()]
    return out


def gpu_libs() -> dict:
    """Where the GPU libraries come from, if anywhere."""
    from . import cuda
    if cuda.bundled_dirs():
        return {"ok": True, "source": "bundled with the app", "dir": None}
    if all((CUDA_DIR / pkg.split("-")[1] / "bin").is_dir() for pkg, _, _ in GPU_PACKAGES) and \
            _has_all_split(CUDA_DIR):
        return {"ok": True, "source": "downloaded by StreamTranslator", "dir": str(CUDA_DIR)}
    saved = _saved_dirs()
    if saved:
        return {"ok": True, "source": saved[1], "dir": saved[0]}
    for d, label in _candidate_dirs():
        if d.is_dir() and _has_all(d):
            return {"ok": True, "source": f"reusing {label}", "dir": str(d), "found": True}
    return {"ok": False, "source": None, "dir": None, "size_gb": 1.3}


def _has_all_split(root: Path) -> bool:
    names = {p.name for p in root.glob("*/bin/*.dll")}
    return all(n in names for n in NEEDED)


_SAVED = BASE / "gpu_libs.json"


def _saved_dirs() -> tuple[str, str] | None:
    try:
        d = json.loads(_SAVED.read_text(encoding="utf-8"))
        if Path(d["dir"]).is_dir() and _has_all(Path(d["dir"])):
            return d["dir"], d["source"]
    except (OSError, ValueError, KeyError):
        pass
    return None


def use_existing(folder: str, label: str) -> None:
    BASE.mkdir(parents=True, exist_ok=True)
    _SAVED.write_text(json.dumps({"dir": folder, "source": label}), encoding="utf-8")


def download_gpu_libs(progress: Callable[[float, str], None] = lambda f, s: None,
                      cancel: threading.Event | None = None) -> None:
    """Download NVIDIA's official cuBLAS/cuDNN wheels from PyPI and keep only the DLLs."""
    plan = []
    for pkg, ver, _ in GPU_PACKAGES:
        req = urllib.request.Request(f"https://pypi.org/pypi/{pkg}/{ver}/json", headers={"User-Agent": "StreamTranslator"})
        with urllib.request.urlopen(req, timeout=30) as r:
            meta = json.loads(r.read().decode())
        wheel = next(u for u in meta["urls"] if u["filename"].endswith("win_amd64.whl"))
        plan.append((pkg, wheel))
    total = sum(w["size"] for _, w in plan)
    done = 0
    CUDA_DIR.mkdir(parents=True, exist_ok=True)
    for pkg, wheel in plan:
        tmp = CUDA_DIR / (wheel["filename"] + ".part")
        h = hashlib.sha256()
        req = urllib.request.Request(wheel["url"], headers={"User-Agent": "StreamTranslator"})
        with urllib.request.urlopen(req, timeout=60) as r, open(tmp, "wb") as f:
            while True:
                if cancel is not None and cancel.is_set():
                    f.close()
                    tmp.unlink(missing_ok=True)
                    raise RuntimeError("Cancelled")
                b = r.read(1 << 20)
                if not b:
                    break
                f.write(b)
                h.update(b)
                done += len(b)
                progress(done / total * 0.95, f"Downloading {pkg} ({done / 1e9:.2f} of {total / 1e9:.2f} GB)")
        if h.hexdigest() != wheel["digests"]["sha256"]:
            tmp.unlink(missing_ok=True)
            raise RuntimeError(f"{pkg} failed its checksum. Nothing was installed; try again.")
        progress(done / total * 0.95, f"Unpacking {pkg}…")
        sub = pkg.split("-")[1]                       # cublas / cudnn
        dest = CUDA_DIR / sub / "bin"
        dest.mkdir(parents=True, exist_ok=True)
        with zipfile.ZipFile(tmp) as z:
            for m in z.namelist():
                if m.lower().endswith(".dll") and "/bin/" in m and ".." not in m:
                    with z.open(m) as src, open(dest / Path(m).name, "wb") as out:
                        shutil.copyfileobj(src, out)
        tmp.unlink(missing_ok=True)
    progress(1.0, "GPU libraries ready")


# ---------------------------------------------------------------- models

def model_status() -> dict:
    from .pipeline import model_is_downloaded
    from .translate import REPOS, is_downloaded
    out = {"whisper": {k: {"label": lbl, "gb": gb, "ok": model_is_downloaded(k)}
                       for k, (_, gb, lbl) in WHISPER_MODELS.items()},
           "translator": {k: {"gb": gb, "ok": is_downloaded(k)} for k, (_, gb) in REPOS.items()}}
    from . import isolate
    out["isolation"] = {"gb": 0.07, "ok": isolate.model_ready()}
    return out


def _hf_cache_size(repo: str) -> int:
    root = Path(os.environ.get("HF_HOME", Path.home() / ".cache" / "huggingface")) / "hub"
    total = 0
    for p in root.rglob("*"):
        try:
            if p.is_file() and (repo.replace("/", "--") in str(p) or p.suffix == ".incomplete"):
                total += p.stat().st_size
        except OSError:
            pass
    return total


def download_model(kind: str, name: str, progress: Callable[[float, str], None] = lambda f, s: None) -> None:
    """Download a Whisper or translator model, reporting progress by watching the cache grow."""
    from huggingface_hub import snapshot_download
    if kind == "whisper":
        repo, gb, label = WHISPER_MODELS[name]
        title = f"Whisper {label}"
    elif kind == "translator":
        from .translate import REPOS
        repo, gb = REPOS[name]
        title = "Translator"
    else:
        from . import isolate
        isolate.download_model(lambda f: progress(f, "Voice isolation model"))
        return
    stop = threading.Event()
    start = _hf_cache_size(repo)

    def watch():
        while not stop.wait(0.7):
            got = _hf_cache_size(repo) - start
            progress(min(0.99, got / (gb * 1e9)), f"Downloading {title} ({got / 1e9:.2f} of ~{gb} GB)")
    t = threading.Thread(target=watch, daemon=True)
    t.start()
    try:
        snapshot_download(repo, allow_patterns=["*.bin", "*.json", "*.model", "*.txt"])
    finally:
        stop.set()
    progress(1.0, f"{title} ready")


def check_model_folder(folder: str) -> str:
    """A faster-whisper (CTranslate2) model folder: model.bin + config.json + tokenizer."""
    p = Path(folder)
    if not (p / "model.bin").exists() or not (p / "config.json").exists():
        if (p / "pytorch_model.bin").exists() or any(p.glob("*.pt")):
            raise ValueError("That's an OpenAI/PyTorch Whisper model. This app needs a faster-whisper "
                             "(CTranslate2) model: a folder with model.bin and config.json.")
        if any(p.glob("ggml*.bin")):
            raise ValueError("That's a whisper.cpp model, which this app can't use. It needs a faster-whisper "
                             "(CTranslate2) model folder with model.bin and config.json.")
        raise ValueError("That folder doesn't look like a faster-whisper model (it needs model.bin and config.json).")
    if not ((p / "tokenizer.json").exists() or (p / "vocabulary.json").exists() or (p / "vocabulary.txt").exists()):
        raise ValueError("The model folder is missing its tokenizer/vocabulary file.")
    return str(p)


def ffmpeg() -> dict:
    from .pipeline import JobError, ffmpeg_exe
    try:
        path = ffmpeg_exe()
        bundled = getattr(sys, "frozen", False) and str(Path(getattr(sys, "_MEIPASS", ""))) in path
        return {"ok": True, "source": "bundled with the app" if bundled else "installed on this PC", "path": path}
    except JobError:
        return {"ok": False}


def status() -> dict:
    return {"gpu": gpu_libs(), "models": model_status(), "ffmpeg": ffmpeg(), "has_nvidia": _has_nvidia()}


def _has_nvidia() -> bool:
    try:
        r = subprocess.run(["nvidia-smi", "-L"], capture_output=True, text=True, timeout=5, creationflags=NO_WINDOW)
        return r.returncode == 0 and "GPU" in r.stdout
    except Exception:
        return False
