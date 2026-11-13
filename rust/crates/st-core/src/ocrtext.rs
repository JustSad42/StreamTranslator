//! Rules for text read off the screen: player nameplates (detect.players_in) and the in-game clock
//! (snap.clock_values, snap._minute_variants).

use std::{
    collections::{BTreeSet, HashSet},
    sync::LazyLock,
};

use fancy_regex::Regex as FRegex;
use regex::Regex;

use crate::glossary::{norm, COMMON};

static NAMEPLATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([A-Z][A-Z0-9]{1,4})\s+([A-Za-z0-9][A-Za-z0-9_.\-]{1,15})\b").unwrap());
static T1_FIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(T[lI1|])[\s_]").unwrap());
static TEAM_UNDERSCORE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b([A-Z][A-Z0-9]{1,4})_").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z][A-Za-z0-9]{2,13}").unwrap());

const HUD_WORDS: &[&str] = &[
    "patch", "live", "replay", "semifinals", "finals", "quarterfinals", "baron", "dragon", "elder", "herald", "gold",
    "kills", "towers", "victory", "defeat", "worlds", "msi", "nearsighted", "game", "lck", "lpl", "lec", "lcs", "ljl",
    "pcs", "vcs", "cblol", "lta", "blg", "geng", "hle", "jdg", "tes", "kc", "fnc", "g2", "t1", "kt", "dk", "drx", "fox",
    "ns", "bro", "you", "the", "and", "for", "all", "team", "red", "blue", "level", "shop", "ping", "fps", "stats", "kda",
    "subs", "follow", "donate",
];

fn has_cased(s: &str) -> bool {
    s.chars().any(|c| c.is_alphabetic())
}
/// Python's str.islower()
fn is_lower(s: &str) -> bool {
    has_cased(s) && !s.chars().any(|c| c.is_uppercase())
}
/// Python's str.isupper()
fn is_upper(s: &str) -> bool {
    has_cased(s) && !s.chars().any(|c| c.is_lowercase())
}

fn capitalize(w: &str) -> String {
    let mut cs = w.chars();
    match cs.next() {
        Some(f) => f.to_uppercase().chain(cs.flat_map(|c| c.to_lowercase())).collect(),
        None => String::new(),
    }
}

/// Counts in first-seen order (like collections.Counter + most_common, ties by first appearance).
#[derive(Default)]
struct Counter(Vec<(String, usize)>);

impl Counter {
    fn add(&mut self, w: &str) {
        match self.0.iter_mut().find(|e| e.0 == w) {
            Some(e) => e.1 += 1,
            None => self.0.push((w.to_string(), 1)),
        }
    }
    fn most_common(&self) -> Vec<(String, usize)> {
        let mut v = self.0.clone();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    }
}

/// Player names: "T1 Faker"-style nameplates, plus short names on screen (scoreboard) in most frames.
/// `ocr_lines`: per frame, the lines OCR read. `champion_keys`: normalised champion names to skip.
pub fn players_in(ocr_lines: &[Vec<String>], champion_keys: &HashSet<String>) -> Vec<String> {
    let ok = |name: &str| {
        let key = name.to_lowercase();
        let n = name.chars().count();
        (3..=14).contains(&n)
            && !name.chars().all(|c| c.is_ascii_digit())
            && name.chars().filter(|c| c.is_ascii_digit()).count() <= 2
            && !COMMON.contains(key.as_str())
            && !HUD_WORDS.contains(&key.as_str())
            && !champion_keys.contains(&norm(name))
    };
    let mut strong = Counter::default();
    let mut seen = Counter::default();
    for lines in ocr_lines {
        let (mut here_strong, mut here_words): (Vec<String>, Vec<String>) = (vec![], vec![]);
        for line in lines {
            let line = T1_FIX.replace_all(line, "T1 ");
            let line = TEAM_UNDERSCORE.replace_all(&line, "$1 ");
            for c in NAMEPLATE.captures_iter(&line) {
                let (team, name) = (&c[1], &c[2]);
                if ok(name) && !["LPL", "LCK", "LEC", "LCS", "GG", "MVP", "KDA", "CS"].contains(&team) {
                    let mut cs = name.chars();
                    let v: String = cs.next().unwrap().to_uppercase().chain(cs).collect();
                    if !here_strong.contains(&v) {
                        here_strong.push(v);
                    }
                }
            }
            for m in WORD.find_iter(&line) {
                let w = m.as_str();
                let rest: String = w.chars().skip(1).collect();
                if ok(w) && w.chars().all(|c| c.is_alphabetic()) && w.chars().next().unwrap().is_uppercase() && (is_lower(&rest) || (is_upper(w) && w.len() <= 6)) {
                    let v = if is_upper(w) { capitalize(w) } else { w.to_string() };
                    if !here_words.contains(&v) {
                        here_words.push(v);
                    }
                }
            }
        }
        here_strong.iter().for_each(|w| strong.add(w));
        here_words.iter().for_each(|w| seen.add(w));
    }
    let n = ocr_lines.len();
    let mut names: Vec<String> = strong.most_common().into_iter().filter(|(_, c)| *c >= if n > 2 { 2 } else { 1 }).map(|e| e.0).collect();
    for (w, c) in seen.most_common() {
        if c >= (n.saturating_sub(1)).max(2) && !names.contains(&w) {
            names.push(w);
        }
    }
    names.truncate(12);
    names
}

static CLOCK: LazyLock<FRegex> = LazyLock::new(|| FRegex::new(r"(?<![\d:])(\d{1,2})[:;.](\d{2})(?![\d:])").unwrap());

fn fix_ocr_digits(w: &str) -> String {
    w.chars()
        .map(|c| match c {
            'O' | 'o' | 'D' => '0',
            'l' | 'I' | '|' => '1',
            'S' => '5',
            'B' => '8',
            'Z' => '2',
            c => c,
        })
        .collect()
}

/// Every plausible in-game time (seconds) in OCR text. "h:mm:ss" (stream uptime) is not one.
pub fn clock_values(lines: &[String]) -> Vec<u32> {
    let mut out = vec![];
    for line in lines {
        let fixed: Vec<String> = line
            .split_whitespace()
            .map(|w| if w.chars().filter(|c| c.is_ascii_digit()).count() >= 2 { fix_ocr_digits(w) } else { w.to_string() })
            .collect();
        let fixed = fixed.join(" ");
        for c in CLOCK.captures_iter(&fixed).flatten() {
            let mm: u32 = c[1].parse().unwrap();
            let ss: u32 = c[2].parse().unwrap();
            if ss < 60 && mm < 80 {
                out.push(mm * 60 + ss);
            }
        }
    }
    out
}

/// Digits OCR commonly swaps on game fonts (a "7" read as "1", "6" as "8"...).
const CONFUSE: &[(char, &str)] = &[('1', "7"), ('7', "1"), ('6', "85"), ('8', "630"), ('5', "6"), ('3', "8"), ('0', "8")];

/// The reading plus versions with one minute digit swapped (seconds cross-check themselves).
pub fn minute_variants(g: u32) -> Vec<u32> {
    let (mm, ss) = (g / 60, g % 60);
    let s: Vec<char> = format!("{mm:02}").chars().collect();
    let mut out = BTreeSet::from([g]);
    for (i, ch) in s.iter().enumerate() {
        if let Some((_, alts)) = CONFUSE.iter().find(|c| c.0 == *ch) {
            for alt in alts.chars() {
                let mut t = s.clone();
                t[i] = alt;
                let v: u32 = t.iter().collect::<String>().parse().unwrap();
                if v < 80 {
                    out.insert(v * 60 + ss);
                }
            }
        }
    }
    out.into_iter().collect()
}
