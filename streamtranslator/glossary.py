"""League of Legends vocabulary.

Two jobs:
1. `hotwords()` - a short list of terms handed to Whisper so it spells game words
   (champions, objectives, summoner spells) instead of guessing normal words.
2. `fix()` - cleans up the English text afterwards: leftover foreign game terms
   ("Heraldo" -> "Herald"), mangled champion names ("Nafiri" -> "Naafiri",
   "Kaisa" -> "Kai'Sa"), and your own wrong -> right fixes.

The champion list comes from Riot's public Data Dragon and is cached, so new
champions show up on their own. Your words and fixes live in
%APPDATA%/StreamTranslator/glossary.json.
"""
from __future__ import annotations

import difflib
import json
import os
import re
import threading
import time
import urllib.request
from pathlib import Path

DATA_DIR = Path(os.environ.get("APPDATA", Path.home())) / "StreamTranslator"
GLOSSARY_FILE = DATA_DIR / "glossary.json"
CHAMP_CACHE = DATA_DIR / "champions.json"
DDRAGON = "https://ddragon.leagueoflegends.com"

FALLBACK_CHAMPIONS = [
    "Aatrox", "Ahri", "Akali", "Akshan", "Alistar", "Ambessa", "Amumu", "Anivia", "Annie", "Aphelios",
    "Ashe", "Aurelion Sol", "Aurora", "Azir", "Bard", "Bel'Veth", "Blitzcrank", "Brand", "Braum", "Briar",
    "Caitlyn", "Camille", "Cassiopeia", "Cho'Gath", "Corki", "Darius", "Diana", "Dr. Mundo", "Draven",
    "Ekko", "Elise", "Evelynn", "Ezreal", "Fiddlesticks", "Fiora", "Fizz", "Galio", "Gangplank", "Garen",
    "Gnar", "Gragas", "Graves", "Gwen", "Hecarim", "Heimerdinger", "Hwei", "Illaoi", "Irelia", "Ivern",
    "Janna", "Jarvan IV", "Jax", "Jayce", "Jhin", "Jinx", "K'Sante", "Kai'Sa", "Kalista", "Karma",
    "Karthus", "Kassadin", "Katarina", "Kayle", "Kayn", "Kennen", "Kha'Zix", "Kindred", "Kled", "Kog'Maw",
    "LeBlanc", "Lee Sin", "Leona", "Lillia", "Lissandra", "Lucian", "Lulu", "Lux", "Malphite", "Malzahar",
    "Maokai", "Master Yi", "Mel", "Milio", "Miss Fortune", "Mordekaiser", "Morgana", "Naafiri", "Nami",
    "Nasus", "Nautilus", "Neeko", "Nidalee", "Nilah", "Nocturne", "Nunu & Willump", "Olaf", "Orianna",
    "Ornn", "Pantheon", "Poppy", "Pyke", "Qiyana", "Quinn", "Rakan", "Rammus", "Rek'Sai", "Rell",
    "Renata Glasc", "Renekton", "Rengar", "Riven", "Rumble", "Ryze", "Samira", "Sejuani", "Senna",
    "Seraphine", "Sett", "Shaco", "Shen", "Shyvana", "Singed", "Sion", "Sivir", "Skarner", "Smolder",
    "Sona", "Soraka", "Swain", "Sylas", "Syndra", "Tahm Kench", "Taliyah", "Talon", "Taric", "Teemo",
    "Thresh", "Tristana", "Trundle", "Tryndamere", "Twisted Fate", "Twitch", "Udyr", "Urgot", "Varus",
    "Vayne", "Veigar", "Vel'Koz", "Vex", "Vi", "Viego", "Viktor", "Vladimir", "Volibear", "Warwick",
    "Wukong", "Xayah", "Xerath", "Xin Zhao", "Yasuo", "Yone", "Yorick", "Yunara", "Yuumi", "Zaahen", "Zac",
    "Zed", "Zeri", "Ziggs", "Zilean", "Zoe", "Zyra",
]

# Most useful first: only the first ~600 characters fit in Whisper's prompt.
CORE_TERMS = [
    "Baron", "Dragon", "Elder Dragon", "Rift Herald", "Void Grubs", "Atakhan", "Nexus", "inhibitor",
    "turret", "jungler", "gank", "Flash", "Teleport", "Ignite", "Smite", "Exhaust", "ult", "ADC",
    "support", "top lane", "mid lane", "bot lane", "ward", "Control Ward", "solo kill", "first blood",
    "Pentakill", "shutdown", "Scuttle", "Blue buff", "Red buff", "Zhonya's", "Guardian Angel",
    "drake", "Soul", "cooldown", "CS", "wave", "recall", "dive", "engage", "peel", "kite", "poke",
    "all-in", "outplay", "ace", "Krugs", "Gromp", "Raptors", "solo queue", "LP", "Challenger",
    "Grandmaster", "LCK", "LPL", "LEC", "LCS", "Worlds", "MSI", "int", "tilted", "gg", "ff",
]

# Foreign game words Whisper leaves untranslated, and literal translations of game terms.
TERM_FIXES = {
    "Herald of the Rift": "Rift Herald", "Heraldo del Rift": "Rift Herald", "Heraldo": "Herald",
    "Heraldos": "Heralds", "Arauto do Vale": "Rift Herald", "Arauto": "Herald", "Héraut": "Herald",
    "Herold": "Herald", "Barón": "Baron", "Barão": "Baron", "the Nashor": "Baron",
    "Elder Drake": "Elder Dragon", "Dragón Ancestral": "Elder Dragon", "dragón": "dragon",
    "dragão": "dragon", "Void Larvae": "Void Grubs", "void larvae": "Void Grubs", "larvae": "grubs",
    "larvas": "grubs", "Vastilarvas": "Void Grubs", "Larves du Néant": "Void Grubs",
    "Ataján": "Atakhan", "Atakan": "Atakhan", "Nexo": "Nexus", "inhibidor": "inhibitor",
    "inibidor": "inhibitor", "torreta": "turret", "ulti": "ult", "jungla": "jungle", "Teleporto": "Teleport",
    "A.D.C.": "ADC", "Pentakil": "Pentakill", "Penta kill": "Pentakill",
}

# Gaming slang the text translator takes literally ("de menos a más" -> "from less to more").
# Swapped for plain English before translating; NLLB keeps English words in a sentence as they are.
# Keys are regexes (matched whole-word, any case).
SLANG = {
    "es": {
        r"de menos a m[aá]s": "getting better", r"jugad[oó]n(?:azo)?": "great play", r"jugadas? de crack": "great play",
        r"chetad[oa]s?": "overpowered", r"rot[oa]s?(?= (?:el|la|ese|esa|este|esta)\b)": "overpowered",
        r"tilte(?:ad[oa]s?|o)": "tilted", r"tiltear(?:se)?": "tilt", r"(?:un|una) manc[oa]": "a bad player", r"manc[oa]s": "bad players", r"manc[oa]": "bad",
        r"rushe(?:ar|a|amos|an)": "rush", r"farme(?:ar|a|ando|amos)": "farm", r"gankee?(?:ar|a|ó|o|ando)": "gank",
        r"flashe(?:ar|a|ó|o)": "flash", r"kite(?:ar|a|ando)": "kite", r"pushe(?:ar|a|ando|amos)": "push",
        r"bai?te(?:ar|a|ó|o|ando)": "bait", r"carre(?:ar|a|ó|o|ando)": "carry", r"feede(?:ar|a|ando)": "feed",
        r"feedead[oa]s?": "fed", r"stune(?:ar|a|ó|o)": "stun", r"snowballe(?:ar|a|ando)": "snowball",
        r"tryhardear": "tryhard", r"nerfe(?:ar|aron|ado)": "nerf", r"bufe(?:ar|aron|ado)": "buff",
        r"rotar": "rotate", r"rotando": "rotating", r"dive(?:ar|a|ando)": "dive", r"tankear": "tank",
        r"(?:lo|la) (?:ha |han )?reventad[oa]": "destroyed him", r"(?:est[aá]n? )?on fire": "on fire",
        r"ez|izi": "easy", r"gg ?wp": "good game, well played",
        r"qu[eé] locura": "that's insane", r"qu[eé] bestia": "what a beast",
    },
    "pt": {
        r"amass(?:ou|ando|ar|a)": "stomped", r"mit(?:ou|ando|ar)": "went crazy", r"jogad(?:aç)?a(?:ço)?": "great play",
        r"tiltad[oa]s?": "tilted", r"tiltar": "tilt", r"rushar": "rush", r"farm(?:ar|ando|ou)": "farm",
        r"gank(?:ar|ou|ando)": "gank", r"flash(?:ar|ou)": "flash", r"kit(?:ar|ando)": "kite", r"push(?:ar|ando)": "push",
        r"carreg(?:ou|ar|ando)": "carried", r"feed(?:ar|ou|ando)": "feed", r"roubad[oa]": "overpowered",
        r"apel[ãa]o|apelona": "overpowered", r"bagre": "bad player", r"tá?nkar": "tank", r"diva(?:r|ndo|ou)": "dive",
        r"izi": "easy", r"gg ?wp": "good game, well played",
    },
    "fr": {
        r"tilt[ée]s?": "tilted", r"rush(?:er|e)": "rush", r"farm(?:er|e)": "farm", r"gank(?:er|e)": "gank",
        r"feed(?:er|e)": "feed", r"carr(?:y|ie)r": "carry", r"cheat[ée]": "overpowered", r"abus[ée]": "overpowered",
    },
    "de": {
        r"getiltet": "tilted", r"gefeedet": "fed", r"gecarried": "carried", r"gegankt": "ganked", r"op": "overpowered",
    },
}

# Mishearings that a plain spelling match can't catch.
ALIASES = {
    "casante": "K'Sante", "kasante": "K'Sante", "nafiri": "Naafiri", "nafili": "Naafiri",
    "leeshin": "Lee Sin", "monkeyking": "Wukong", "kaisa": "Kai'Sa", "kaiza": "Kai'Sa",
    "khazix": "Kha'Zix", "chogath": "Cho'Gath", "velkoz": "Vel'Koz", "reksai": "Rek'Sai",
    "kogmaw": "Kog'Maw", "belveth": "Bel'Veth", "ksante": "K'Sante", "jarvan": "Jarvan IV",
    "missfortuna": "Miss Fortune", "aurelionsoul": "Aurelion Sol", "tahmkench": "Tahm Kench",
    "masteryi": "Master Yi", "xinzao": "Xin Zhao", "nunu": "Nunu", "mundo": "Mundo",
}

# Capitalised English words that sit close to a champion name. Never "fix" these.
COMMON = set("""
there these their where which while maybe right great nice again still never since night light fight
might ready really thank please sorry about after again along always another around because before
being below between could doing during every first found going heard house later least leave little
looks maybe minus money mother never nothing other place point power quite rather really right round
second seven shall short should since small sound start still story sure taken thank thing think those
three today under until using voice watch water where while whole whose world would write years young
yeah okay alright guys bro dude chat stream again brand rumble kindred talon graves twitch aurora vex
smolder riven thresh fizz lux bard senna ivern diana leona janna quinn sion zoe mel victor sienna
""".split())


def _norm(s: str) -> str:
    return re.sub(r"[^a-z0-9]", "", s.lower().replace("’", "'"))


class Glossary:
    _instance: "Glossary | None" = None
    _lock = threading.Lock()

    @classmethod
    def get(cls) -> "Glossary":
        with cls._lock:
            if cls._instance is None:
                cls._instance = Glossary()
            return cls._instance

    def __init__(self):
        self.enabled = True
        self.use_core_terms = True
        self.custom_words: list[str] = []
        self.fixes: dict[str, str] = {}
        self.use_slang = True
        self.slang: dict[str, str] = {}          # the user's own: phrase in any language -> plain English
        self.champions: list[dict] = [{"name": n, "id": None} for n in FALLBACK_CHAMPIONS]
        self.version: str | None = None
        self.updated = 0.0
        self._load()
        self._build()

    # ---- storage
    def _load(self):
        try:
            d = json.loads(GLOSSARY_FILE.read_text(encoding="utf-8"))
            self.enabled = bool(d.get("enabled", True))
            self.use_core_terms = bool(d.get("use_core_terms", True))
            self.custom_words = [w for w in d.get("custom_words", []) if isinstance(w, str) and w.strip()]
            self.fixes = {k: v for k, v in (d.get("fixes") or {}).items() if k.strip()}
            self.use_slang = bool(d.get("use_slang", True))
            self.slang = {k: v for k, v in (d.get("slang") or {}).items() if k.strip()}
        except (OSError, ValueError):
            pass
        try:
            c = json.loads(CHAMP_CACHE.read_text(encoding="utf-8"))
            if c.get("champions"):
                self.champions, self.version, self.updated = c["champions"], c.get("version"), c.get("updated", 0)
        except (OSError, ValueError):
            pass

    def save(self):
        DATA_DIR.mkdir(parents=True, exist_ok=True)
        GLOSSARY_FILE.write_text(json.dumps({
            "enabled": self.enabled, "use_core_terms": self.use_core_terms,
            "custom_words": self.custom_words, "fixes": self.fixes,
            "use_slang": self.use_slang, "slang": self.slang,
        }, indent=2, ensure_ascii=False), encoding="utf-8")
        self._build()

    def update(self, data: dict):
        if "enabled" in data:
            self.enabled = bool(data["enabled"])
        if "use_core_terms" in data:
            self.use_core_terms = bool(data["use_core_terms"])
        if "custom_words" in data:
            seen, words = set(), []
            for w in data["custom_words"]:
                w = str(w).strip()
                if w and w.lower() not in seen:
                    seen.add(w.lower())
                    words.append(w)
            self.custom_words = words
        if "fixes" in data:
            self.fixes = {str(k).strip(): str(v).strip() for k, v in data["fixes"].items() if str(k).strip()}
        if "use_slang" in data:
            self.use_slang = bool(data["use_slang"])
        if "slang" in data:
            self.slang = {str(k).strip(): str(v).strip() for k, v in data["slang"].items() if str(k).strip()}
        self.save()

    def refresh_champions(self, force: bool = False) -> bool:
        """Pull the current champion list from Data Dragon (cached for 3 days)."""
        if not force and self.version and time.time() - self.updated < 3 * 86400:
            return False
        def get(url):
            req = urllib.request.Request(url, headers={"User-Agent": "StreamTranslator"})
            with urllib.request.urlopen(req, timeout=10) as r:
                return json.loads(r.read().decode("utf-8"))
        version = get(f"{DDRAGON}/api/versions.json")[0]
        data = get(f"{DDRAGON}/cdn/{version}/data/en_US/champion.json")["data"]
        champs = sorted(({"name": c["name"], "id": c["id"]} for c in data.values()), key=lambda c: c["name"])
        self.champions, self.version, self.updated = champs, version, time.time()
        DATA_DIR.mkdir(parents=True, exist_ok=True)
        CHAMP_CACHE.write_text(json.dumps({"version": version, "updated": self.updated, "champions": champs},
                                          ensure_ascii=False), encoding="utf-8")
        self._build()
        return True

    def icon_url(self, champ_id: str | None) -> str | None:
        if champ_id and self.version:
            return f"{DDRAGON}/cdn/{self.version}/img/champion/{champ_id}.png"
        return None

    def summary(self) -> dict:
        return {
            "enabled": self.enabled, "use_core_terms": self.use_core_terms,
            "custom_words": self.custom_words, "fixes": self.fixes, "version": self.version,
            "use_slang": self.use_slang, "slang": self.slang,
            "builtin_slang": sum(len(v) for v in SLANG.values()),
            "core_terms": CORE_TERMS,
            "champions": [{"name": c["name"], "id": c.get("id"), "icon": self.icon_url(c.get("id"))}
                          for c in self.champions],
        }

    # ---- matching tables
    def _build(self):
        names = [c["name"] for c in self.champions] + self.custom_words
        self._exact = {_norm(n): n for n in names if len(_norm(n)) >= 3}
        for k, v in ALIASES.items():
            self._exact.setdefault(k, v)
        self._fuzzy_keys = [k for k in self._exact if len(k) >= 4]
        self._fix_res = [(re.compile(r"(?<!\w)" + re.escape(w) + r"(?!\w)", re.I), r)
                         for w, r in sorted(self.fixes.items(), key=lambda kv: -len(kv[0]))]
        self._term_res = [(re.compile(r"(?<!\w)" + re.escape(w) + r"(?!\w)", re.I), r)
                          for w, r in sorted(TERM_FIXES.items(), key=lambda kv: -len(kv[0]))]
        self._slang_user = [(re.compile(r"(?<!\w)" + re.escape(w) + r"(?!\w)", re.I), r)
                            for w, r in sorted(self.slang.items(), key=lambda kv: -len(kv[0]))]
        self._slang_builtin = {lang: [(re.compile(r"(?<!\w)(?:" + w + r")(?!\w)", re.I), r) for w, r in table.items()]
                               for lang, table in SLANG.items()}

    # ---- before translating
    def prepare(self, text: str, lang: str | None) -> tuple[str, int]:
        """Swap gaming slang for plain English before the text translator sees it."""
        if not self.enabled or not self.use_slang or not text:
            return text, 0
        n = 0
        for rx, rep in self._slang_user + self._slang_builtin.get(lang or "", []):
            text, k = rx.subn(lambda m, r=rep: r[:1].upper() + r[1:] if m.group(0)[:1].isupper() else r, text)
            n += k
        return text, n

    # ---- Whisper prompt
    def hotwords(self, in_game: list[str] | None = None, limit: int = 600, core: bool = True,
                 extra: list[str] | None = None) -> str | None:
        """Terms for Whisper's prompt. core=False leaves out the English game terms (used when
        Whisper writes down the original language, where English terms would only confuse it)."""
        if not self.enabled:
            return None
        words: list[str] = []
        terms = CORE_TERMS if (self.use_core_terms and core) else []
        for w in list(in_game or []) + list(extra or []) + self.custom_words + terms:
            if w not in words:
                words.append(w)
        out = ""
        for w in words:
            nxt = f"{out}, {w}" if out else w
            if len(nxt) > limit:
                break
            out = nxt
        return out or None

    def is_prompt_echo(self, text: str, hotwords: str | None) -> bool:
        """Whisper sometimes repeats the hotword list back during silence."""
        if not hotwords or text.count(",") < 3:
            return False
        hw = {_norm(w) for w in hotwords.split(",")}
        parts = [_norm(p) for p in text.split(",") if p.strip()]
        return bool(parts) and sum(p in hw for p in parts) / len(parts) >= 0.6

    # ---- cleanup
    def fix(self, text: str, english: bool = True, extra: list[str] | None = None) -> tuple[str, int]:
        """extra: names for this job only (e.g. players read off the screen)."""
        if not self.enabled or not text:
            return text, 0
        if extra:
            text, k = self._fix_names(text, {_norm(n): n for n in extra if len(_norm(n)) >= 3})
            fixed, n = self.fix(text, english)
            return fixed, n + k
        n = 0
        for rx, rep in self._fix_res:
            text, k = rx.subn(rep, text)
            n += k
        if not english:
            return text, n
        for rx, rep in self._term_res:
            text, k = rx.subn(rep, text)
            n += k
        text, k = self._fix_names(text)
        return text, n + k

    def _fix_names(self, text: str, table: dict[str, str] | None = None) -> tuple[str, int]:
        exact = table if table is not None else self._exact
        fuzzy_keys = [k for k in exact if len(k) >= 4] if table is not None else self._fuzzy_keys
        tokens = list(re.finditer(r"[A-Za-zÀ-ÿ][A-Za-zÀ-ÿ'’.&]*", text))
        edits: list[tuple[int, int, str]] = []
        i = 0
        while i < len(tokens):
            t = tokens[i]
            # two-word names first ("Lee Shin", "Miss Fortuna")
            if i + 1 < len(tokens):
                t2 = tokens[i + 1]
                span = text[t.start():t2.end()]
                if t2.start() - t.end() <= 1:
                    hit = exact.get(_norm(span))
                    if hit and span != hit and span.lower() != hit.lower():
                        edits.append((t.start(), t2.end(), hit))
                        i += 2
                        continue
            word = t.group().rstrip(".'’")
            key = _norm(word)
            hit = exact.get(key)
            if hit:
                if word != hit and word.lower() != hit.lower():
                    edits.append((t.start(), t.start() + len(word), hit))
            elif word[:1].isupper() and len(key) >= 5 and key not in COMMON:
                close = difflib.get_close_matches(key, fuzzy_keys, n=1, cutoff=0.84)
                if close:
                    edits.append((t.start(), t.start() + len(word), exact[close[0]]))
            i += 1
        for s, e, rep in reversed(edits):
            text = text[:s] + rep + text[e:]
        return text, len(edits)
