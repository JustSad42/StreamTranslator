"""Live Twitch chat, translated to English on your GPU.

Reads chat anonymously (Twitch allows read-only "justinfan" logins, no account
needed). Messages are translated with Meta's NLLB-200 model running locally
through CTranslate2 (downloaded once, ~1.2 GB). Emotes are left alone, and
messages that are already English are shown as they are.
"""
from __future__ import annotations

import random
import re
import socket
import ssl
import threading
import time
from typing import Callable

from .translate import NLLB_CODES, Translator  # noqa: F401  (shared with two-step speech translation)
# Common third-party emotes (7TV/BTTV/FFZ) that Twitch doesn't tag.
EMOTES = set("""KEKW OMEGALUL LUL LULW Pog PogU PogChamp POGGERS Poggers monkaS monkaW Sadge Copium COPIUM Aware
Clueless Kappa KappaPride ResidentSleeper PepeHands PepeLaugh peepoHappy FeelsBadMan FeelsGoodMan FeelsStrongMan
EZ EZY Clap catJAM pepeD widepeepoHappy Kreygasm BibleThump NotLikeThis WutFace 4Head HeyGuys TriHard
SeemsGood VoHiYo CoolStoryBob D: xdd xd XD Deadge Stare ICANT Susge Chatting GIGACHAD Gigachad OkaygeBusiness
Okayge BASED NOWAYING NOOO forsenCD ratJAM peepoClap HUH Erm BOOBA monkaHmm Hmm modCheck WAYTOODANK""".split())
ENGLISH = set("""the a an and or but is are was were be been i you he she it we they my your his her its our their
this that these those what why how who when where yes no not lol lmao omg gg wp go good bad nice wow ok okay
please thanks thank love so very just like can can't don't do does did have has had will would should could
to of in on at for with from up down out im it's that's he's what's bro man dude fr ngl team game win lose""".split())


# Laughter and keyboard smashes in many languages ("jajaja", "kkkk", "jsjsj", "hahaha", "wwww", "555", "ㅋㅋ").
# The translator invents whole sentences for these, so they're shown as they are.
_LAUGH = re.compile(r"(?:rs)+r?|k{3,}|w{3,}|5{3,}|x+d+|l+o+l+|[ㅋㅎ]+|[笑w草]+|a?s+d+f*|[!?.]+", re.I)
_LAUGH_LETTERS = set("jahesk wx")


def is_laugh(word: str) -> bool:
    w = word.lower().strip(".,!?¡¿")
    if len(w) < 2:
        return False
    if _LAUGH.fullmatch(w) or w in ("xd", "jaja", "jeje", "haha", "hehe"):
        return True
    # "jajaja", "jsjsjs", "JSJWKSKKAKAKA": only laugh letters, with j/h/k repeated
    return len(w) >= 4 and set(w) <= _LAUGH_LETTERS and sum(w.count(c) for c in "jhk") >= 2


def strip_laughs(text: str) -> str:
    return " ".join(w for w in text.split() if not is_laugh(w))


def is_english(text: str) -> bool:
    words = re.findall(r"[a-zA-Z']+", text.lower())
    if not words:
        return True
    if len(re.findall(r"[^\x00-\x7f]", text)) > len(text) * 0.2:
        return False
    return sum(w in ENGLISH for w in words) / len(words) >= 0.4


class ChatSession:
    """Join a Twitch channel's chat and emit translated messages."""

    def __init__(self, channel: str, get_lang: Callable[[], str | None], emit: Callable[..., None],
                 size: str = "better"):
        self.size = size
        self.channel = channel.lower().lstrip("#")
        self.get_lang = get_lang
        self.emit = emit
        self.stop_event = threading.Event()
        self.queue: list[dict] = []
        self.qlock = threading.Lock()
        self.sock: socket.socket | None = None

    def start(self):
        threading.Thread(target=self._irc, daemon=True).start()
        threading.Thread(target=self._worker, daemon=True).start()

    def stop(self):
        self.stop_event.set()
        try:
            self.sock and self.sock.close()
        except OSError:
            pass

    # ---- IRC
    def _irc(self):
        backoff = 1
        while not self.stop_event.is_set():
            try:
                raw = socket.create_connection(("irc.chat.twitch.tv", 6697), timeout=30)
                self.sock = ssl.create_default_context().wrap_socket(raw, server_hostname="irc.chat.twitch.tv")
                nick = f"justinfan{random.randint(10000, 99999)}"
                for line in ("CAP REQ :twitch.tv/tags", "PASS SCHMOOPIIE", f"NICK {nick}", f"JOIN #{self.channel}"):
                    self.sock.sendall((line + "\r\n").encode())
                self.emit("chat.status", state="connected", channel=self.channel)
                backoff = 1
                buf = b""
                self.sock.settimeout(300)
                while not self.stop_event.is_set():
                    data = self.sock.recv(65536)
                    if not data:
                        break
                    buf += data
                    *lines, buf = buf.split(b"\r\n")
                    for ln in lines:
                        self._line(ln.decode("utf-8", "replace"))
            except OSError:
                pass
            if self.stop_event.is_set():
                break
            self.emit("chat.status", state="reconnecting", channel=self.channel)
            time.sleep(backoff)
            backoff = min(backoff * 2, 30)

    def _line(self, ln: str):
        if ln.startswith("PING"):
            self.sock.sendall(("PONG" + ln[4:] + "\r\n").encode())
            return
        m = re.match(r"^(?:@(\S+) )?:(\w+)!\S+ PRIVMSG #\w+ :(.*)$", ln)
        if not m:
            return
        tags = dict(kv.split("=", 1) for kv in (m.group(1) or "").split(";") if "=" in kv)
        text = m.group(3)
        if text.startswith("\x01ACTION "):
            text = text[8:].rstrip("\x01")
        # blank out Twitch emotes by their tagged positions
        plain = list(text)
        for spec in (tags.get("emotes") or "").split("/"):
            if ":" in spec:
                for rng in spec.split(":", 1)[1].split(","):
                    a, b = (int(x) for x in rng.split("-"))
                    for i in range(a, min(b + 1, len(plain))):
                        plain[i] = " "
        words = [w for w in "".join(plain).split() if w not in EMOTES and not w.startswith("@")]
        msg = {"id": tags.get("id") or str(time.time()), "user": tags.get("display-name") or m.group(2),
               "color": tags.get("color") or "", "text": text, "en": None}
        core = strip_laughs(" ".join(words))
        if len(core) < 3 or not re.search(r"[^\W\d_]{2}", core) or is_english(core):
            self.emit("chat.message", **msg)
            return
        msg["core"] = core
        with self.qlock:
            if len(self.queue) > 150:      # very busy chat: show the oldest untranslated
                old = self.queue.pop(0)
                old.pop("core", None)
                self.emit("chat.message", **old)
            self.queue.append(msg)

    # ---- translation
    def _worker(self):
        try:
            tr = Translator.get(self.size, lambda s: self.emit("chat.status", state="loading", message=s, channel=self.channel))
            self.emit("chat.status", state="ready", channel=self.channel, device=tr.device)
        except Exception as e:
            self.emit("chat.status", state="error", message=f"Chat translator failed to load: {e}", channel=self.channel)
            tr = None
        while not self.stop_event.wait(0.4):
            with self.qlock:
                batch, self.queue = self.queue[:24], self.queue[24:]
            if not batch:
                continue
            lang = self.get_lang()
            outs = [None] * len(batch)
            if tr and lang and lang != "en":
                try:
                    outs = tr.translate([m["core"] for m in batch], lang)
                except Exception:
                    pass
            for m, en in zip(batch, outs):
                m.pop("core", None)
                m["en"] = en if en and en.strip().lower() != m["text"].strip().lower() else None
                self.emit("chat.message", **m)
