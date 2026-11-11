"""Tell speakers apart: who said each line (streamer, duo partner, co-caster...).

Each spoken line gets a voice fingerprint from WeSpeaker's ResNet34 model (ONNX, 26 MB,
downloaded once, checksum-verified). Lines whose fingerprints are close are grouped; the
person who talks most is speaker 1. Runs on the CPU in a few seconds per 10 minutes of speech.
"""
from __future__ import annotations

import hashlib
import os
import threading
import urllib.request
from pathlib import Path
from typing import Callable

import numpy as np

MODEL_URL = "https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM/resolve/main/voxceleb_resnet34_LM.onnx"
MODEL_SHA256 = "7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068"
MODEL_PATH = Path(os.environ.get("LOCALAPPDATA", Path.home())) / "StreamTranslator" / "models" / "wespeaker-resnet34.onnx"

SR = 16000
SAME = 0.3          # line-level cosine similarity for grouping (stream audio is noisy)
MERGE = 0.3         # groups whose average voices are this close are the same person
MIN_LINE = 0.8      # seconds; shorter lines take the speaker of their neighbours
MAX_LINE = 8.0      # seconds of each line used for its fingerprint
MINOR = 0.04        # a group with less than this share of speech time and <= 2 lines is folded into the closest

_session = None
_lock = threading.Lock()


def model_ready() -> bool:
    return MODEL_PATH.exists() and MODEL_PATH.stat().st_size > 20_000_000


def download_model(progress: Callable[[float], None] = lambda f: None):
    if model_ready():
        return
    MODEL_PATH.parent.mkdir(parents=True, exist_ok=True)
    tmp = MODEL_PATH.with_suffix(".part")
    h = hashlib.sha256()
    req = urllib.request.Request(MODEL_URL, headers={"User-Agent": "StreamTranslator"})
    with urllib.request.urlopen(req, timeout=60) as r, open(tmp, "wb") as f:
        size = int(r.headers.get("Content-Length") or 26_530_309)
        got = 0
        while b := r.read(1 << 20):
            f.write(b)
            h.update(b)
            got += len(b)
            progress(got / size)
    if h.hexdigest() != MODEL_SHA256:
        tmp.unlink(missing_ok=True)
        raise RuntimeError("The speaker model failed its checksum. Try again.")
    tmp.replace(MODEL_PATH)


def _get_session():
    global _session
    with _lock:
        if _session is None:
            import onnxruntime as ort
            opts = ort.SessionOptions()
            opts.intra_op_num_threads = max(2, (os.cpu_count() or 4) // 2)
            _session = ort.InferenceSession(str(MODEL_PATH), opts, providers=["CPUExecutionProvider"])
        return _session


# ---------------------------------------------------------------- Kaldi-style log-mel filterbank
_N, _HOP, _NFFT, _MELS = 400, 160, 512, 80


def _mel_banks() -> np.ndarray:
    mel = lambda f: 1127.0 * np.log(1.0 + f / 700.0)
    lo, hi = mel(20.0), mel(SR / 2)
    centers = np.linspace(lo, hi, _MELS + 2)
    fft_mel = mel(np.arange(_NFFT // 2 + 1) * SR / _NFFT)
    banks = np.zeros((_MELS, _NFFT // 2 + 1), np.float32)
    for m in range(_MELS):
        l, c, r = centers[m], centers[m + 1], centers[m + 2]
        up = (fft_mel - l) / (c - l)
        down = (r - fft_mel) / (r - c)
        banks[m] = np.clip(np.minimum(up, down), 0, None)
    return banks


_BANKS = _mel_banks()
_WINDOW = ((0.5 - 0.5 * np.cos(2 * np.pi * np.arange(_N) / (_N - 1))) ** 0.85).astype(np.float32)


def fbank(x: np.ndarray) -> np.ndarray:
    """80-dim log-mel features (25 ms frames, 10 ms hop), mean-normalised. x: 16 kHz float in -1..1."""
    x = x.astype(np.float32) * 32768.0
    if len(x) < _N:
        x = np.pad(x, (0, _N - len(x)))
    n = 1 + (len(x) - _N) // _HOP
    idx = np.arange(_N)[None, :] + _HOP * np.arange(n)[:, None]
    fr = x[idx]
    fr = fr - fr.mean(axis=1, keepdims=True)
    fr = np.concatenate([fr[:, :1] * (1 - 0.97), fr[:, 1:] - 0.97 * fr[:, :-1]], axis=1)
    fr = fr * _WINDOW
    spec = np.abs(np.fft.rfft(fr, _NFFT, axis=1)) ** 2
    feats = np.log(np.maximum(spec @ _BANKS.T, np.finfo(np.float32).eps))
    return (feats - feats.mean(axis=0, keepdims=True)).astype(np.float32)


def embed(x: np.ndarray) -> np.ndarray:
    feats = fbank(x)[None]
    e = _get_session().run(["embs"], {"feats": feats})[0][0]
    return e / (np.linalg.norm(e) + 1e-9)


# ---------------------------------------------------------------- grouping
def cluster(embs: np.ndarray, weights: np.ndarray, same: float = SAME) -> np.ndarray:
    """Average-linkage grouping by cosine similarity. Returns a group number per row."""
    n = len(embs)
    if n <= 1:
        return np.zeros(n, int)
    sim = (embs @ embs.T).astype(np.float64)
    np.fill_diagonal(sim, -np.inf)
    size = np.ones(n)
    alive = np.ones(n, bool)
    labels = np.arange(n)
    while alive.sum() > 1:
        flat = np.argmax(sim)
        a, b = divmod(int(flat), n)
        if sim[a, b] < same:
            break
        # average linkage: the merged group's similarity to k is the size-weighted mean
        merged = (size[a] * sim[a] + size[b] * sim[b]) / (size[a] + size[b])
        sim[a], sim[:, a] = merged, merged
        sim[a, a] = -np.inf
        sim[b], sim[:, b] = -np.inf, -np.inf
        size[a] += size[b]
        alive[b] = False
        labels[labels == b] = a
    # Single lines are noisy (game sound, short phrases), so one person often ends up in several
    # groups. Group averages are much steadier: merge groups whose averages are close.
    while True:
        ids = sorted(set(labels))
        if len(ids) <= 1:
            break
        cent = np.array([(embs[labels == g] * weights[labels == g, None]).sum(0) for g in ids])
        cent /= np.linalg.norm(cent, axis=1, keepdims=True) + 1e-9
        cs = cent @ cent.T
        np.fill_diagonal(cs, -1)
        a, b = divmod(int(np.argmax(cs)), len(ids))
        if cs[a, b] < MERGE:
            break
        labels[labels == ids[b]] = ids[a]
    # fold tiny groups (a one-off shout, a champion voice line) into the closest real one
    total = weights.sum() or 1.0
    while True:
        ids = sorted(set(labels))
        if len(ids) <= 1:
            break
        share = {g: weights[labels == g].sum() / total for g in ids}
        small = [g for g in ids if share[g] < MINOR and (labels == g).sum() <= 2]
        if not small:
            break
        g = min(small, key=lambda k: share[k])
        others = [k for k in ids if k != g]
        cent = {k: embs[labels == k].mean(axis=0) for k in ids}
        near = max(others, key=lambda k: float(cent[g] @ cent[k]))
        labels[labels == g] = near
    return labels


def label(audio: np.ndarray, spans: list[tuple[float, float]], progress: Callable[[float], None] = lambda f: None,
          cancel=None) -> list[int]:
    """Speaker number (0 = talks most) for each (start, end) span in seconds of `audio`."""
    if not spans:
        return []
    keep, embs, weights = [], [], []
    for i, (s, e) in enumerate(spans):
        if cancel is not None and cancel.is_set():
            from .pipeline import Cancelled
            raise Cancelled()
        if e - s >= MIN_LINE:
            mid = (s + e) / 2
            half = min(e - s, MAX_LINE) / 2
            chunk = audio[int((mid - half) * SR):int((mid + half) * SR)]
            if len(chunk) >= int(MIN_LINE * SR):
                keep.append(i)
                embs.append(embed(chunk))
                weights.append(e - s)
        progress((i + 1) / len(spans))
    out = [-1] * len(spans)
    if keep:
        labels = cluster(np.array(embs), np.array(weights))
        talk = {g: sum(w for w, l in zip(weights, labels) if l == g) for g in set(labels)}
        order = {g: n for n, g in enumerate(sorted(talk, key=lambda g: -talk[g]))}
        for i, l in zip(keep, labels):
            out[i] = order[l]
    # short lines: same speaker as the closest line in time that has one
    known = [i for i, v in enumerate(out) if v >= 0]
    for i, v in enumerate(out):
        if v < 0:
            out[i] = out[min(known, key=lambda k: abs(k - i))] if known else 0
    return out


# subtitle colours for speakers 2, 3, ... (speaker 1 stays white)
COLORS = ["#FFD54F", "#4FC3F7", "#A5D6A7", "#F48FB1", "#CE93D8", "#FFAB91"]


def color(n: int) -> str | None:
    return None if n <= 0 else COLORS[(n - 1) % len(COLORS)]
