//! League of Legends vocabulary (glossary.py).
//!
//! 1. `hotwords()`: terms handed to Whisper so it spells game words instead of guessing.
//! 2. `prepare()`: gaming slang swapped for plain English before NLLB translates.
//! 3. `fix()`: cleans up the text afterwards (foreign game terms, mangled champion names, user fixes).
//!
//! Stored in %APPDATA%\StreamTranslator\glossary.json (shared with the Python app; unknown keys are kept).

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::LazyLock,
};

use fancy_regex::{Captures, NoExpand, Regex};
use serde_json::{Map, Value};

use crate::py;

pub const DDRAGON: &str = "https://ddragon.leagueoflegends.com";

pub const FALLBACK_CHAMPIONS: &[&str] = &[
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
];

/// Most useful first: only the first ~600 characters fit in Whisper's prompt.
pub const CORE_TERMS: &[&str] = &[
    "Baron", "Dragon", "Elder Dragon", "Rift Herald", "Void Grubs", "Atakhan", "Nexus", "inhibitor",
    "turret", "jungler", "gank", "Flash", "Teleport", "Ignite", "Smite", "Exhaust", "ult", "ADC",
    "support", "top lane", "mid lane", "bot lane", "ward", "Control Ward", "solo kill", "first blood",
    "Pentakill", "shutdown", "Scuttle", "Blue buff", "Red buff", "Zhonya's", "Guardian Angel",
    "drake", "Soul", "cooldown", "CS", "wave", "recall", "dive", "engage", "peel", "kite", "poke",
    "all-in", "outplay", "ace", "Krugs", "Gromp", "Raptors", "solo queue", "LP", "Challenger",
    "Grandmaster", "LCK", "LPL", "LEC", "LCS", "Worlds", "MSI", "int", "tilted", "gg", "ff",
];

/// Foreign game words Whisper leaves untranslated, and literal translations of game terms.
pub const TERM_FIXES: &[(&str, &str)] = &[
    ("Herald of the Rift", "Rift Herald"), ("Heraldo del Rift", "Rift Herald"), ("Heraldo", "Herald"),
    ("Heraldos", "Heralds"), ("Arauto do Vale", "Rift Herald"), ("Arauto", "Herald"), ("Héraut", "Herald"),
    ("Herold", "Herald"), ("Barón", "Baron"), ("Barão", "Baron"), ("the Nashor", "Baron"),
    ("Elder Drake", "Elder Dragon"), ("Dragón Ancestral", "Elder Dragon"), ("dragón", "dragon"),
    ("dragão", "dragon"), ("Void Larvae", "Void Grubs"), ("void larvae", "Void Grubs"), ("larvae", "grubs"),
    ("larvas", "grubs"), ("Vastilarvas", "Void Grubs"), ("Larves du Néant", "Void Grubs"),
    ("Ataján", "Atakhan"), ("Atakan", "Atakhan"), ("Nexo", "Nexus"), ("inhibidor", "inhibitor"),
    ("inibidor", "inhibitor"), ("torreta", "turret"), ("ulti", "ult"), ("jungla", "jungle"), ("Teleporto", "Teleport"),
    ("A.D.C.", "ADC"), ("Pentakil", "Pentakill"), ("Penta kill", "Pentakill"),
];

/// Gaming slang NLLB takes literally ("de menos a más" -> "from less to more"), swapped for plain English
/// first. Keys are regexes, matched whole-word in any case.
pub const SLANG: &[(&str, &[(&str, &str)])] = &[
    ("es", &[
        (r"de menos a m[aá]s", "getting better"), (r"jugad[oó]n(?:azo)?", "great play"), (r"jugadas? de crack", "great play"),
        (r"chetad[oa]s?", "overpowered"), (r"rot[oa]s?(?= (?:el|la|ese|esa|este|esta)\b)", "overpowered"),
        (r"tilte(?:ad[oa]s?|o)", "tilted"), (r"tiltear(?:se)?", "tilt"), (r"(?:un|una) manc[oa]", "a bad player"), (r"manc[oa]s", "bad players"), (r"manc[oa]", "bad"),
        (r"rushe(?:ar|a|amos|an)", "rush"), (r"farme(?:ar|a|ando|amos)", "farm"), (r"gankee?(?:ar|a|ó|o|ando)", "gank"),
        (r"flashe(?:ar|a|ó|o)", "flash"), (r"kite(?:ar|a|ando)", "kite"), (r"pushe(?:ar|a|ando|amos)", "push"),
        (r"bai?te(?:ar|a|ó|o|ando)", "bait"), (r"carre(?:ar|a|ó|o|ando)", "carry"), (r"feede(?:ar|a|ando)", "feed"),
        (r"feedead[oa]s?", "fed"), (r"stune(?:ar|a|ó|o)", "stun"), (r"snowballe(?:ar|a|ando)", "snowball"),
        (r"tryhardear", "tryhard"), (r"nerfe(?:ar|aron|ado)", "nerf"), (r"bufe(?:ar|aron|ado)", "buff"),
        (r"rotar", "rotate"), (r"rotando", "rotating"), (r"dive(?:ar|a|ando)", "dive"), (r"tankear", "tank"),
        (r"(?:lo|la) (?:ha |han )?reventad[oa]", "destroyed him"), (r"(?:est[aá]n? )?on fire", "on fire"),
        (r"ez|izi", "easy"), (r"gg ?wp", "good game, well played"),
        (r"qu[eé] locura", "that's insane"), (r"qu[eé] bestia", "what a beast"),
    ]),
    ("pt", &[
        (r"amass(?:ou|ando|ar|a)", "stomped"), (r"mit(?:ou|ando|ar)", "went crazy"), (r"jogad(?:aç)?a(?:ço)?", "great play"),
        (r"tiltad[oa]s?", "tilted"), (r"tiltar", "tilt"), (r"rushar", "rush"), (r"farm(?:ar|ando|ou)", "farm"),
        (r"gank(?:ar|ou|ando)", "gank"), (r"flash(?:ar|ou)", "flash"), (r"kit(?:ar|ando)", "kite"), (r"push(?:ar|ando)", "push"),
        (r"carreg(?:ou|ar|ando)", "carried"), (r"feed(?:ar|ou|ando)", "feed"), (r"roubad[oa]", "overpowered"),
        (r"apel[ãa]o|apelona", "overpowered"), (r"bagre", "bad player"), (r"tá?nkar", "tank"), (r"diva(?:r|ndo|ou)", "dive"),
        (r"izi", "easy"), (r"gg ?wp", "good game, well played"),
    ]),
    ("fr", &[
        (r"tilt[ée]s?", "tilted"), (r"rush(?:er|e)", "rush"), (r"farm(?:er|e)", "farm"), (r"gank(?:er|e)", "gank"),
        (r"feed(?:er|e)", "feed"), (r"carr(?:y|ie)r", "carry"), (r"cheat[ée]", "overpowered"), (r"abus[ée]", "overpowered"),
    ]),
    ("de", &[
        (r"getiltet", "tilted"), (r"gefeedet", "fed"), (r"gecarried", "carried"), (r"gegankt", "ganked"), (r"op", "overpowered"),
    ]),
];

/// Mishearings that a plain spelling match can't catch.
pub const ALIASES: &[(&str, &str)] = &[
    ("casante", "K'Sante"), ("kasante", "K'Sante"), ("nafiri", "Naafiri"), ("nafili", "Naafiri"),
    ("leeshin", "Lee Sin"), ("monkeyking", "Wukong"), ("kaisa", "Kai'Sa"), ("kaiza", "Kai'Sa"),
    ("khazix", "Kha'Zix"), ("chogath", "Cho'Gath"), ("velkoz", "Vel'Koz"), ("reksai", "Rek'Sai"),
    ("kogmaw", "Kog'Maw"), ("belveth", "Bel'Veth"), ("ksante", "K'Sante"), ("jarvan", "Jarvan IV"),
    ("missfortuna", "Miss Fortune"), ("aurelionsoul", "Aurelion Sol"), ("tahmkench", "Tahm Kench"),
    ("masteryi", "Master Yi"), ("xinzao", "Xin Zhao"), ("nunu", "Nunu"), ("mundo", "Mundo"),
];

/// Capitalised English words that sit close to a champion name. Never "fix" these.
pub static COMMON: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    "there these their where which while maybe right great nice again still never since night light fight
might ready really thank please sorry about after again along always another around because before
being below between could doing during every first found going heard house later least leave little
looks maybe minus money mother never nothing other place point power quite rather really right round
second seven shall short should since small sound start still story sure taken thank thing think those
three today under until using voice watch water where while whole whose world would write years young
yeah okay alright guys bro dude chat stream again brand rumble kindred talon graves twitch aurora vex
smolder riven thresh fizz lux bard senna ivern diana leona janna quinn sion zoe mel victor sienna"
        .split_whitespace()
        .collect()
});

static NON_ALNUM: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[^a-z0-9]").unwrap());
static TOKEN: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"[A-Za-zÀ-ÿ][A-Za-zÀ-ÿ'’.&]*").unwrap());

/// Lower-case letters and digits only ("K'Sante" -> "ksante").
pub fn norm(s: &str) -> String {
    NON_ALNUM.replace_all(&s.to_lowercase().replace('’', "'"), "").into_owned()
}

fn word_re(literal: &str) -> Regex {
    Regex::new(&format!(r"(?i)(?<!\w){}(?!\w)", fancy_regex::escape(literal))).unwrap()
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Champion {
    pub name: String,
    #[serde(default)]
    pub id: Option<String>,
}

/// A table of correct names by their normalised spelling.
struct Names {
    exact: HashMap<String, String>,
    fuzzy: Vec<String>,
}

impl Names {
    fn new(pairs: impl IntoIterator<Item = (String, String)>, aliases: bool) -> Names {
        let mut exact = HashMap::new();
        let mut order = vec![];
        for (k, v) in pairs {
            if !exact.contains_key(&k) {
                order.push(k.clone());
            }
            exact.insert(k, v);
        }
        if aliases {
            for (k, v) in ALIASES {
                if !exact.contains_key(*k) {
                    order.push(k.to_string());
                    exact.insert(k.to_string(), v.to_string());
                }
            }
        }
        let fuzzy = order.into_iter().filter(|k| k.len() >= 4).collect();
        Names { exact, fuzzy }
    }
}

pub struct Glossary {
    pub enabled: bool,
    pub use_core_terms: bool,
    pub custom_words: Vec<String>,
    /// the user's wrong -> right fixes (in file order)
    pub fixes: Vec<(String, String)>,
    pub use_slang: bool,
    /// the user's own slang: phrase in any language -> plain English
    pub slang: Vec<(String, String)>,
    pub champions: Vec<Champion>,
    /// Data Dragon version of the cached champion list
    pub version: Option<String>,
    pub updated: f64,
    /// keys in glossary.json this version doesn't know (kept on save)
    extra: Map<String, Value>,
    dir: PathBuf,
    names: Names,
    fix_res: Vec<(Regex, String)>,
    term_res: Vec<(Regex, String)>,
    slang_user: Vec<(Regex, String)>,
    slang_builtin: HashMap<&'static str, Vec<(Regex, String)>>,
}

fn str_pairs(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(|v| v.as_object())
        .map(|m| m.iter().filter(|(k, _)| !k.trim().is_empty()).map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect())
        .unwrap_or_default()
}

impl Glossary {
    /// Built-in defaults only (no files).
    pub fn defaults() -> Glossary {
        let mut g = Glossary {
            enabled: true,
            use_core_terms: true,
            custom_words: vec![],
            fixes: vec![],
            use_slang: true,
            slang: vec![],
            champions: FALLBACK_CHAMPIONS.iter().map(|n| Champion { name: n.to_string(), id: None }).collect(),
            version: None,
            updated: 0.0,
            extra: Map::new(),
            dir: PathBuf::new(),
            names: Names::new([], true),
            fix_res: vec![],
            term_res: vec![],
            slang_user: vec![],
            slang_builtin: HashMap::new(),
        };
        g.build();
        g
    }

    /// glossary.json + champions.json from a data folder (%APPDATA%\StreamTranslator).
    pub fn load(dir: &Path) -> Glossary {
        let mut g = Glossary::defaults();
        g.dir = dir.to_path_buf();
        if let Some(Value::Object(mut d)) = std::fs::read(dir.join("glossary.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            let b = |v: Option<&Value>| v.and_then(|v| v.as_bool()).unwrap_or(true);
            g.enabled = b(d.get("enabled"));
            g.use_core_terms = b(d.get("use_core_terms"));
            g.custom_words = d
                .get("custom_words")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|w| w.as_str()).filter(|w| !w.trim().is_empty()).map(String::from).collect())
                .unwrap_or_default();
            g.fixes = str_pairs(d.get("fixes"));
            g.use_slang = b(d.get("use_slang"));
            g.slang = str_pairs(d.get("slang"));
            for k in ["enabled", "use_core_terms", "custom_words", "fixes", "use_slang", "slang"] {
                d.remove(k);
            }
            g.extra = d;
        }
        if let Some(c) = std::fs::read(dir.join("champions.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            if let Ok(list) = serde_json::from_value::<Vec<Champion>>(c["champions"].clone()) {
                if !list.is_empty() {
                    g.champions = list;
                    g.version = c["version"].as_str().map(String::from);
                    g.updated = c["updated"].as_f64().unwrap_or(0.0);
                }
            }
        }
        g.build();
        g
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let mut m = Map::new();
        m.insert("enabled".into(), self.enabled.into());
        m.insert("use_core_terms".into(), self.use_core_terms.into());
        m.insert("custom_words".into(), self.custom_words.clone().into());
        m.insert("fixes".into(), Value::Object(self.fixes.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect()));
        m.insert("use_slang".into(), self.use_slang.into());
        m.insert("slang".into(), Value::Object(self.slang.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect()));
        for (k, v) in &self.extra {
            m.insert(k.clone(), v.clone());
        }
        py::write_text(&self.dir.join("glossary.json"), &py::json_dumps(&Value::Object(m), Some(2), false))?;
        self.build();
        Ok(())
    }

    /// Apply edits from the Glossary tab and save (Glossary.update).
    pub fn update(&mut self, data: &Value) -> std::io::Result<()> {
        let truthy = |v: &Value| match v {
            Value::Bool(b) => *b,
            Value::Null => false,
            Value::Number(n) => n.as_f64() != Some(0.0),
            Value::String(s) => !s.is_empty(),
            Value::Array(a) => !a.is_empty(),
            Value::Object(o) => !o.is_empty(),
        };
        let text = |v: &Value| match v {
            Value::String(s) => s.trim().to_string(),
            other => other.to_string().trim().to_string(),
        };
        let pairs = |v: &Value| -> Vec<(String, String)> {
            v.as_object().map(|m| m.iter().map(|(k, v)| (k.trim().to_string(), text(v))).filter(|(k, _)| !k.is_empty()).collect()).unwrap_or_default()
        };
        if let Some(v) = data.get("enabled") {
            self.enabled = truthy(v);
        }
        if let Some(v) = data.get("use_core_terms") {
            self.use_core_terms = truthy(v);
        }
        if let Some(words) = data.get("custom_words").and_then(|v| v.as_array()) {
            let mut seen = HashSet::new();
            self.custom_words = words.iter().map(text).filter(|w| !w.is_empty() && seen.insert(w.to_lowercase())).collect();
        }
        if let Some(v) = data.get("fixes") {
            self.fixes = pairs(v);
        }
        if let Some(v) = data.get("use_slang") {
            self.use_slang = truthy(v);
        }
        if let Some(v) = data.get("slang") {
            self.slang = pairs(v);
        }
        self.save()
    }

    /// A newer champion list from Data Dragon: keep it and cache it in champions.json.
    pub fn set_champions(&mut self, champions: Vec<Champion>, version: String, updated: f64) -> std::io::Result<()> {
        self.champions = champions;
        self.version = Some(version.clone());
        self.updated = updated;
        std::fs::create_dir_all(&self.dir)?;
        let v = serde_json::json!({"version": version, "updated": py::f(updated), "champions": self.champions});
        py::write_text(&self.dir.join("champions.json"), &py::json_dumps(&v, None, false))?;
        self.build();
        Ok(())
    }

    pub fn icon_url(&self, champ_id: Option<&str>) -> Option<String> {
        match (champ_id, &self.version) {
            (Some(id), Some(v)) if !id.is_empty() => Some(format!("{DDRAGON}/cdn/{v}/img/champion/{id}.png")),
            _ => None,
        }
    }

    /// What the Glossary tab shows.
    pub fn summary(&self) -> Value {
        let pairs = |p: &[(String, String)]| Value::Object(p.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect());
        serde_json::json!({
            "enabled": self.enabled, "use_core_terms": self.use_core_terms,
            "custom_words": self.custom_words, "fixes": pairs(&self.fixes), "version": self.version,
            "use_slang": self.use_slang, "slang": pairs(&self.slang),
            "builtin_slang": SLANG.iter().map(|(_, t)| t.len()).sum::<usize>(),
            "core_terms": CORE_TERMS,
            "champions": self.champions.iter().map(|c| serde_json::json!({"name": c.name, "id": c.id, "icon": self.icon_url(c.id.as_deref())})).collect::<Vec<_>>(),
        })
    }

    /// Rebuild the matching tables (after changing words, fixes or champions).
    pub fn build(&mut self) {
        let names = self.champions.iter().map(|c| c.name.clone()).chain(self.custom_words.iter().cloned());
        self.names = Names::new(names.filter_map(|n| (norm(&n).len() >= 3).then(|| (norm(&n), n))), true);
        let sorted = |pairs: Vec<(String, String)>| {
            let mut p = pairs;
            p.sort_by_key(|(k, _)| std::cmp::Reverse(k.chars().count()));
            p.into_iter().map(|(k, v)| (word_re(&k), v)).collect::<Vec<_>>()
        };
        self.fix_res = sorted(self.fixes.clone());
        self.term_res = sorted(TERM_FIXES.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect());
        self.slang_user = sorted(self.slang.clone());
        self.slang_builtin = SLANG
            .iter()
            .map(|(lang, table)| {
                let res = table.iter().map(|(w, r)| (Regex::new(&format!(r"(?i)(?<!\w)(?:{w})(?!\w)")).unwrap(), r.to_string())).collect();
                (*lang, res)
            })
            .collect();
    }

    /// Swap gaming slang for plain English before the text translator sees it.
    pub fn prepare(&self, text: &str, lang: Option<&str>) -> (String, usize) {
        if !self.enabled || !self.use_slang || text.is_empty() {
            return (text.to_string(), 0);
        }
        let mut text = text.to_string();
        let mut n = 0;
        let builtin = self.slang_builtin.get(lang.unwrap_or("")).map(|v| v.as_slice()).unwrap_or(&[]);
        for (rx, rep) in self.slang_user.iter().chain(builtin) {
            let mut k = 0;
            let out = rx.replace_all(&text, |c: &Captures| {
                k += 1;
                if c[0].chars().next().is_some_and(|ch| ch.is_uppercase()) {
                    let mut cs = rep.chars();
                    cs.next().map(|f| f.to_uppercase().chain(cs).collect()).unwrap_or_default()
                } else {
                    rep.clone()
                }
            });
            text = out.into_owned();
            n += k;
        }
        (text, n)
    }

    /// Terms for Whisper's prompt. `core = false` leaves out the English game terms (used when Whisper
    /// writes down the original language, where English terms would only confuse it).
    pub fn hotwords(&self, in_game: &[String], core: bool, extra: &[String]) -> Option<String> {
        self.hotwords_limit(in_game, core, extra, 600)
    }

    pub fn hotwords_limit(&self, in_game: &[String], core: bool, extra: &[String], limit: usize) -> Option<String> {
        if !self.enabled {
            return None;
        }
        let terms: Vec<String> = if self.use_core_terms && core { CORE_TERMS.iter().map(|s| s.to_string()).collect() } else { vec![] };
        let mut words: Vec<&String> = vec![];
        for w in in_game.iter().chain(extra).chain(&self.custom_words).chain(&terms) {
            if !words.contains(&w) {
                words.push(w);
            }
        }
        let mut out = String::new();
        for w in words {
            let next = if out.is_empty() { w.clone() } else { format!("{out}, {w}") };
            if next.chars().count() > limit {
                break;
            }
            out = next;
        }
        (!out.is_empty()).then_some(out)
    }

    /// Whisper sometimes repeats the hotword list back during silence.
    pub fn is_prompt_echo(&self, text: &str, hotwords: Option<&str>) -> bool {
        let Some(hot) = hotwords else { return false };
        if hot.is_empty() || text.matches(',').count() < 3 {
            return false;
        }
        let hw: HashSet<String> = hot.split(',').map(norm).collect();
        let parts: Vec<String> = text.split(',').filter(|p| !p.trim().is_empty()).map(norm).collect();
        !parts.is_empty() && parts.iter().filter(|p| hw.contains(*p)).count() as f64 / parts.len() as f64 >= 0.6
    }

    /// Clean up a line. `extra`: names for this job only (players read off the screen).
    pub fn fix(&self, text: &str, english: bool, extra: &[String]) -> (String, usize) {
        if !self.enabled || text.is_empty() {
            return (text.to_string(), 0);
        }
        if !extra.is_empty() {
            let table = Names::new(extra.iter().filter_map(|n| (norm(n).len() >= 3).then(|| (norm(n), n.clone()))), false);
            let (text, k) = fix_names(text, &table);
            let (fixed, n) = self.fix(&text, english, &[]);
            return (fixed, n + k);
        }
        let mut text = text.to_string();
        let mut n = 0;
        let mut apply = |res: &[(Regex, String)], text: &mut String| {
            for (rx, rep) in res {
                let k = rx.find_iter(text).flatten().count();
                if k > 0 {
                    *text = rx.replace_all(text, NoExpand(rep)).into_owned();
                    n += k;
                }
            }
        };
        apply(&self.fix_res, &mut text);
        if !english {
            return (text, n);
        }
        apply(&self.term_res, &mut text);
        let (text, k) = fix_names(&text, &self.names);
        (text, n + k)
    }

    /// Normalised champion names (for filtering OCR'd player names).
    pub fn champion_keys(&self) -> HashSet<String> {
        self.champions.iter().map(|c| norm(&c.name)).collect()
    }
}

fn fix_names(text: &str, names: &Names) -> (String, usize) {
    let tokens: Vec<regex::Match> = TOKEN.find_iter(text).collect();
    let mut edits: Vec<(usize, usize, String)> = vec![];
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        // two-word names first ("Lee Shin", "Miss Fortuna")
        if i + 1 < tokens.len() {
            let t2 = tokens[i + 1];
            let span = &text[t.start()..t2.end()];
            if text[t.end()..t2.start()].chars().count() <= 1 {
                if let Some(hit) = names.exact.get(&norm(span)) {
                    if span != hit && span.to_lowercase() != hit.to_lowercase() {
                        edits.push((t.start(), t2.end(), hit.clone()));
                        i += 2;
                        continue;
                    }
                }
            }
        }
        let word = t.as_str().trim_end_matches(['.', '\'', '’']);
        let key = norm(word);
        if let Some(hit) = names.exact.get(&key) {
            if word != hit && word.to_lowercase() != hit.to_lowercase() {
                edits.push((t.start(), t.start() + word.len(), hit.clone()));
            }
        } else if word.chars().next().is_some_and(|c| c.is_uppercase()) && key.len() >= 5 && !COMMON.contains(key.as_str()) {
            if let Some(close) = py::close_match(&key, &names.fuzzy, 0.84) {
                edits.push((t.start(), t.start() + word.len(), names.exact[close].clone()));
            }
        }
        i += 1;
    }
    let mut out = text.to_string();
    for (s, e, rep) in edits.iter().rev() {
        out.replace_range(*s..*e, rep);
    }
    (out, edits.len())
}
