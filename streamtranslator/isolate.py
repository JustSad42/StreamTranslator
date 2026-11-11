"""Voice isolation: strip game sounds, music and alerts so Whisper only hears people.

Uses the UVR MDX-Net "Voc_FT" vocal model (ONNX, ~66 MB, downloaded once) on the GPU
through onnxruntime. Audio is cut into ~6 s windows, turned into a spectrogram, the
model predicts the voice part, and that is turned back into audio.
"""
from __future__ import annotations

import os
import subprocess
import threading
import urllib.request
from pathlib import Path
from typing import Callable

import numpy as np

from .pipeline import NO_WINDOW, Cancelled, JobError, ffmpeg_exe

MODEL_URL = "https://github.com/TRvlvr/model_repo/releases/download/all_public_uvr_models/UVR-MDX-NET-Voc_FT.onnx"
MODEL_SIZE = 66762490
MODEL_DIR = Path(os.environ.get("LOCALAPPDATA", Path.home())) / "StreamTranslator" / "models"
MODEL_PATH = MODEL_DIR / "UVR-MDX-NET-Voc_FT.onnx"

SR = 44100
N_FFT = 7680
HOP = 1024
DIM_F = 3072
DIM_T = 256
CHUNK = HOP * (DIM_T - 1)          # samples per model window
TRIM = N_FFT // 2
GEN = CHUNK - 2 * TRIM             # useful samples per window
_WIN = np.hanning(N_FFT + 1)[:-1].astype(np.float32)   # periodic Hann, like torch

_session = None
_lock = threading.Lock()


def model_ready() -> bool:
    return MODEL_PATH.exists() and MODEL_PATH.stat().st_size == MODEL_SIZE


def download_model(progress: Callable[[float], None] = lambda f: None):
    if model_ready():
        return
    MODEL_DIR.mkdir(parents=True, exist_ok=True)
    tmp = MODEL_PATH.with_suffix(".part")
    req = urllib.request.Request(MODEL_URL, headers={"User-Agent": "StreamTranslator"})
    with urllib.request.urlopen(req, timeout=60) as r, open(tmp, "wb") as f:
        got = 0
        while True:
            b = r.read(1 << 20)
            if not b:
                break
            f.write(b)
            got += len(b)
            progress(got / MODEL_SIZE)
    if tmp.stat().st_size != MODEL_SIZE:
        tmp.unlink(missing_ok=True)
        raise JobError("The voice isolation model didn't download completely. Try again.")
    tmp.replace(MODEL_PATH)


def _get_session():
    global _session
    with _lock:
        if _session is None:
            from . import cuda
            cuda.setup()
            import onnxruntime as ort
            opts = ort.SessionOptions()
            opts.log_severity_level = 3
            providers: list = ["CPUExecutionProvider"]
            if "CUDAExecutionProvider" in ort.get_available_providers():
                # Keep GPU memory small and fixed. Without this onnxruntime grabs several GB and
                # keeps it, Windows starts swapping GPU memory and Whisper slows to a crawl.
                providers.insert(0, ("CUDAExecutionProvider", {
                    "gpu_mem_limit": str(1536 * 1024 * 1024),
                    "arena_extend_strategy": "kSameAsRequested",
                    "cudnn_conv_algo_search": "HEURISTIC",
                    "cudnn_conv_use_max_workspace": "0",
                }))
            _session = ort.InferenceSession(str(MODEL_PATH), opts, providers=providers)
        return _session


def release():
    """Free the model's GPU memory (clip jobs call this when they're done with it)."""
    global _session
    with _lock:
        _session = None
    import gc
    gc.collect()


def device() -> str:
    return "GPU" if "CUDAExecutionProvider" in _get_session().get_providers() else "CPU"


# ---------------------------------------------------------------- spectrograms (torch.stft/istft equivalents)

def _stft(x: np.ndarray) -> np.ndarray:
    """x [N, CHUNK] -> complex [N, F, T]"""
    xp = np.pad(x, ((0, 0), (TRIM, TRIM)), mode="reflect")
    idx = np.arange(N_FFT)[None, :] + HOP * np.arange(DIM_T)[:, None]
    return np.fft.rfft(xp[:, idx] * _WIN, axis=-1).transpose(0, 2, 1)


def _istft(spec: np.ndarray) -> np.ndarray:
    """complex [N, F, T] -> [N, CHUNK]"""
    frames = np.fft.irfft(spec.transpose(0, 2, 1), n=N_FFT, axis=-1) * _WIN
    n, t = frames.shape[0], frames.shape[1]
    out = np.zeros((n, N_FFT + HOP * (t - 1)), dtype=np.float32)
    norm = np.zeros(out.shape[1], dtype=np.float32)
    w2 = _WIN ** 2
    for i in range(t):
        out[:, i * HOP:i * HOP + N_FFT] += frames[:, i]
        norm[i * HOP:i * HOP + N_FFT] += w2
    out /= np.maximum(norm, 1e-8)
    return out[:, TRIM:TRIM + CHUNK]


def _run_windows(windows: np.ndarray) -> np.ndarray:
    """windows [B, 2, CHUNK] stereo -> isolated voice [B, 2, CHUNK]"""
    b = windows.shape[0]
    spec = _stft(windows.reshape(b * 2, CHUNK))[:, :DIM_F]               # [2B, F, T]
    spec[:, :3] = 0                                                      # drop DC bins, as UVR does
    x = np.stack([spec.real, spec.imag], axis=1).reshape(b, 4, DIM_F, DIM_T).astype(np.float32)
    sess = _get_session()
    y = sess.run(None, {sess.get_inputs()[0].name: x})[0]                # [B, 4, F, T]
    y = y.reshape(b * 2, 2, DIM_F, DIM_T)
    full = np.zeros((b * 2, N_FFT // 2 + 1, DIM_T), dtype=np.complex64)
    full[:, :DIM_F] = y[:, 0] + 1j * y[:, 1]
    return _istft(full).reshape(b, 2, CHUNK)


def separate(mix: np.ndarray, progress: Callable[[float], None] = lambda f: None,
             cancel: threading.Event | None = None, batch: int = 1) -> np.ndarray:
    """mix [2, n] at 44.1 kHz -> voice [2, n]"""
    n = mix.shape[1]
    pad = GEN - n % GEN
    padded = np.concatenate([np.zeros((2, TRIM), np.float32), mix, np.zeros((2, pad + TRIM), np.float32)], axis=1)
    starts = list(range(0, n + pad, GEN))
    out = np.zeros((2, n + pad), dtype=np.float32)
    for k in range(0, len(starts), batch):
        if cancel is not None and cancel.is_set():
            raise Cancelled()
        group = starts[k:k + batch]
        res = _run_windows(np.stack([padded[:, s:s + CHUNK] for s in group]))
        for s, r in zip(group, res):
            out[:, s:s + GEN] = r[:, TRIM:TRIM + GEN]
        progress(min(1.0, (k + len(group)) / len(starts)))
    return out[:, :n]


# ---------------------------------------------------------------- helpers for the app

def _ffmpeg_pcm(args_in: list[str], stdin: bytes | None, rate: int, ch: int) -> np.ndarray:
    cmd = [ffmpeg_exe(), "-hide_banner", "-nostdin" if stdin is None else "-hide_banner", "-loglevel", "error",
           *args_in, "-vn", "-ac", str(ch), "-ar", str(rate), "-f", "f32le", "-"]
    r = subprocess.run(cmd, input=stdin, capture_output=True, creationflags=NO_WINDOW)
    if r.returncode != 0:
        raise JobError("Audio conversion failed:\n" + r.stderr.decode(errors="replace")[-500:])
    return np.frombuffer(r.stdout, dtype=np.float32).copy()


BLEED = 0.15     # keep a little of the original: smooths separation artifacts Whisper trips on


def _finish(voice: np.ndarray, mix: np.ndarray) -> np.ndarray:
    """Match the original loudness and mix a touch of it back in (both at 16 kHz mono)."""
    n = min(len(voice), len(mix))
    voice, mix = voice[:n], mix[:n]
    rv, rm = float(np.sqrt(np.mean(voice ** 2)) or 1e-9), float(np.sqrt(np.mean(mix ** 2)) or 1e-9)
    out = voice * (rm / rv) * (1 - BLEED) + mix * BLEED
    peak = float(np.abs(out).max() or 1.0)
    return (out / peak * 0.95 if peak > 0.95 else out).astype(np.float32)


def voice_16k_from_file(path: Path, progress=lambda f: None, cancel=None) -> np.ndarray:
    """Clip file -> isolated voice as 16 kHz mono (what Whisper wants)."""
    stereo = _ffmpeg_pcm(["-i", str(path)], None, SR, 2).reshape(-1, 2).T
    voice = separate(np.ascontiguousarray(stereo), progress, cancel)
    raw = np.ascontiguousarray(voice.T).astype(np.float32).tobytes()
    v16 = _ffmpeg_pcm(["-f", "f32le", "-ar", str(SR), "-ac", "2", "-i", "-"], raw, 16000, 1)
    m16 = _ffmpeg_pcm(["-i", str(path)], None, 16000, 1)
    return _finish(v16, m16)


def _resample(x: np.ndarray, src: int, dst: int) -> np.ndarray:
    n = int(round(len(x) * dst / src))
    if n <= 0 or len(x) == 0:
        return np.zeros(0, np.float32)
    return (np.fft.irfft(np.fft.rfft(x), n=n) * (n / len(x))).astype(np.float32)


def voice_16k_from_16k(x: np.ndarray) -> np.ndarray:
    """Live mode: 16 kHz mono chunk -> isolated 16 kHz mono chunk."""
    up = _resample(x, 16000, SR)
    voice = separate(np.stack([up, up]))
    return _finish(_resample(voice.mean(axis=0), SR, 16000), x)
