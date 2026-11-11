"""Text translation on the GPU with Meta's NLLB-200 (via CTranslate2). Used for:

- two-step speech translation: Whisper writes down what was said in the original
  language (it's much better at that than at translating), then NLLB translates it;
- live chat translation.

Two sizes: "standard" (600M, ~1.2 GB) and "better" (1.3B, ~2.7 GB). Downloaded once
into the shared Hugging Face cache.
"""
from __future__ import annotations

import re
import threading
from typing import Callable

REPOS = {
    "standard": ("JustFrederik/nllb-200-distilled-600M-ct2-float16", 1.2),
    "better": ("JustFrederik/nllb-200-distilled-1.3B-ct2-float16", 2.7),
}
NLLB_CODES = {
    "ar": "arb_Arab", "bg": "bul_Cyrl", "cs": "ces_Latn", "da": "dan_Latn", "de": "deu_Latn", "el": "ell_Grek",
    "es": "spa_Latn", "fi": "fin_Latn", "fr": "fra_Latn", "he": "heb_Hebr", "hi": "hin_Deva", "hr": "hrv_Latn",
    "hu": "hun_Latn", "id": "ind_Latn", "it": "ita_Latn", "ja": "jpn_Jpan", "ko": "kor_Hang", "lt": "lit_Latn",
    "ms": "zsm_Latn", "nl": "nld_Latn", "no": "nob_Latn", "pl": "pol_Latn", "pt": "por_Latn", "ro": "ron_Latn",
    "ru": "rus_Cyrl", "sk": "slk_Latn", "sr": "srp_Cyrl", "sv": "swe_Latn", "th": "tha_Thai", "tl": "tgl_Latn",
    "tr": "tur_Latn", "uk": "ukr_Cyrl", "vi": "vie_Latn", "zh": "zho_Hans",
}
# sentence ends, including CJK punctuation; NLLB tends to drop the tail of multi-sentence input
_SENT = re.compile(r"(?<=[.!?…。！？])\s+|(?<=[。！？])")


def supported(lang: str | None) -> bool:
    return bool(lang) and lang in NLLB_CODES


def is_downloaded(size: str) -> bool:
    try:
        from huggingface_hub import snapshot_download
        snapshot_download(REPOS[size][0], local_files_only=True)
        return True
    except Exception:
        return False


def split_sentences(text: str) -> list[str]:
    parts = [p.strip() for p in _SENT.split(text or "") if p and p.strip()]
    return parts or [text]


class Translator:
    _inst: "Translator | None" = None
    _lock = threading.Lock()

    @classmethod
    def get(cls, size: str = "better", status: Callable[[str], None] = lambda s: None) -> "Translator":
        size = size if size in REPOS else "better"
        with cls._lock:
            if cls._inst is None or cls._inst.size != size:
                cls._inst = None          # free the other size's GPU memory first
                cls._inst = Translator(size, status)
            return cls._inst

    def __init__(self, size: str, status):
        from huggingface_hub import snapshot_download
        from . import cuda
        cuda.setup()
        import ctranslate2
        import sentencepiece as spm
        repo, gb = REPOS[size]
        try:
            path = snapshot_download(repo, local_files_only=True)
        except Exception:
            status(f"Downloading the {'better' if size == 'better' else 'standard'} translator (about {gb} GB, one time)…")
            path = snapshot_download(repo, allow_patterns=["*.bin", "*.json", "*.model", "*.txt"])
        dev = "cuda" if ctranslate2.get_cuda_device_count() > 0 else "cpu"
        self.model = ctranslate2.Translator(path, device=dev, compute_type="float16" if dev == "cuda" else "int8")
        self.sp = spm.SentencePieceProcessor(model_file=f"{path}/sentencepiece.bpe.model")
        self.device = "GPU" if dev == "cuda" else "CPU"
        self.size = size
        self._run = threading.Lock()

    def _batch(self, pieces: list[str], code: str, beam: int) -> list[str]:
        out: list[str] = []
        for i in range(0, len(pieces), 32):
            chunk = pieces[i:i + 32]
            src = [[code] + self.sp.encode(t, out_type=str)[:400] + ["</s>"] for t in chunk]
            with self._run:
                res = self.model.translate_batch(src, target_prefix=[["eng_Latn"]] * len(src), beam_size=beam,
                                                 max_decoding_length=256, repetition_penalty=1.1)
            out += [self.sp.decode(r.hypotheses[0][1:]) for r in res]
        return out

    def translate(self, texts: list[str], src: str, beam: int = 4, by_sentence: bool = True) -> list[str]:
        """Translate each text to English. Multi-sentence texts are split so nothing gets dropped."""
        code = NLLB_CODES.get(src)
        if not code or not texts:
            return list(texts)
        if not by_sentence:
            return self._batch(list(texts), code, beam)
        owners, pieces = [], []
        for i, t in enumerate(texts):
            for s in split_sentences(t):
                owners.append(i)
                pieces.append(s)
        done = self._batch(pieces, code, beam)
        joined = [[] for _ in texts]
        for i, s in zip(owners, done):
            joined[i].append(s.strip())
        return [" ".join(p).strip() for p in joined]
