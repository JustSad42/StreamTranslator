//! Twitch chat text handling (chat.py): laugh filter, "is this English?", emote stripping and the
//! IRC line parser. The network side lives in the app.

use std::{collections::HashSet, sync::LazyLock};

use regex::Regex;

/// Common third-party emotes (7TV/BTTV/FFZ) that Twitch doesn't tag.
pub static EMOTES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    "KEKW OMEGALUL LUL LULW Pog PogU PogChamp POGGERS Poggers monkaS monkaW Sadge Copium COPIUM Aware
Clueless Kappa KappaPride ResidentSleeper PepeHands PepeLaugh peepoHappy FeelsBadMan FeelsGoodMan FeelsStrongMan
EZ EZY Clap catJAM pepeD widepeepoHappy Kreygasm BibleThump NotLikeThis WutFace 4Head HeyGuys TriHard
SeemsGood VoHiYo CoolStoryBob D: xdd xd XD Deadge Stare ICANT Susge Chatting GIGACHAD Gigachad OkaygeBusiness
Okayge BASED NOWAYING NOOO forsenCD ratJAM peepoClap HUH Erm BOOBA monkaHmm Hmm modCheck WAYTOODANK"
        .split_whitespace()
        .collect()
});

static ENGLISH: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    "the a an and or but is are was were be been i you he she it we they my your his her its our their
this that these those what why how who when where yes no not lol lmao omg gg wp go good bad nice wow ok okay
please thanks thank love so very just like can can't don't do does did have has had will would should could
to of in on at for with from up down out im it's that's he's what's bro man dude fr ngl team game win lose"
        .split_whitespace()
        .collect()
});

// Laughter and keyboard smashes in many languages ("jajaja", "kkkk", "jsjsj", "hahaha", "wwww", "555", "ㅋㅋ").
// The translator invents whole sentences for these, so they're shown as they are.
static LAUGH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:(?:rs)+r?|k{3,}|w{3,}|5{3,}|x+d+|l+o+l+|[ㅋㅎ]+|[笑w草]+|a?s+d+f*|[!?.]+)$").unwrap());
const LAUGH_LETTERS: &str = "jahesk wx";

pub fn is_laugh(word: &str) -> bool {
    let low = word.to_lowercase();
    let w = low.trim_matches(|c| ".,!?¡¿".contains(c));
    let n = w.chars().count();
    if n < 2 {
        return false;
    }
    if LAUGH.is_match(w) || ["xd", "jaja", "jeje", "haha", "hehe"].contains(&w) {
        return true;
    }
    // "jajaja", "jsjsjs", "JSJWKSKKAKAKA": only laugh letters, with j/h/k repeated
    n >= 4 && w.chars().all(|c| LAUGH_LETTERS.contains(c)) && w.chars().filter(|c| "jhk".contains(*c)).count() >= 2
}

pub fn strip_laughs(text: &str) -> String {
    text.split_whitespace().filter(|w| !is_laugh(w)).collect::<Vec<_>>().join(" ")
}

pub fn is_english(text: &str) -> bool {
    static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-zA-Z']+").unwrap());
    let low = text.to_lowercase();
    let words: Vec<&str> = WORD.find_iter(&low).map(|m| m.as_str()).collect();
    if words.is_empty() {
        return true;
    }
    let non_ascii = text.chars().filter(|c| !c.is_ascii()).count();
    if non_ascii as f64 > text.chars().count() as f64 * 0.2 {
        return false;
    }
    words.iter().filter(|w| ENGLISH.contains(*w)).count() as f64 / words.len() as f64 >= 0.4
}

/// One chat message as the interface shows it. `core` is the text to translate (None: show as is).
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: String,
    pub user: String,
    pub color: String,
    pub text: String,
    pub core: Option<String>,
}

/// A raw IRC line -> a chat message, or None for anything that isn't one.
pub fn parse_line(ln: &str, now: f64) -> Option<Message> {
    static LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:@(\S+) )?:(\w+)!\S+ PRIVMSG #\w+ :(.*)$").unwrap());
    static LETTERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\W\d_]{2}").unwrap());
    let m = LINE.captures(ln)?;
    let tags: std::collections::HashMap<&str, &str> = m.get(1).map_or("", |t| t.as_str()).split(';').filter_map(|kv| kv.split_once('=')).collect();
    let mut text = m[3].to_string();
    if let Some(rest) = text.strip_prefix("\x01ACTION ") {
        text = rest.trim_end_matches('\x01').to_string();
    }
    // blank out Twitch emotes by their tagged positions (code points)
    let mut plain: Vec<char> = text.chars().collect();
    for spec in tags.get("emotes").copied().unwrap_or("").split('/') {
        if let Some((_, ranges)) = spec.split_once(':') {
            for rng in ranges.split(',') {
                if let Some((a, b)) = rng.split_once('-') {
                    if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                        for c in plain.iter_mut().take((b + 1).min(text.chars().count())).skip(a) {
                            *c = ' ';
                        }
                    }
                }
            }
        }
    }
    let plain: String = plain.into_iter().collect();
    let words: Vec<&str> = plain.split_whitespace().filter(|w| !EMOTES.contains(w) && !w.starts_with('@')).collect();
    let nonempty = |k: &str| tags.get(k).copied().filter(|v| !v.is_empty());
    let core = strip_laughs(&words.join(" "));
    let translate = core.chars().count() >= 3 && LETTERS.is_match(&core) && !is_english(&core);
    Some(Message {
        id: nonempty("id").map(String::from).unwrap_or_else(|| crate::py::float_repr(now)),
        user: nonempty("display-name").unwrap_or(&m[2]).to_string(),
        color: tags.get("color").copied().unwrap_or("").to_string(),
        text,
        core: translate.then_some(core),
    })
}
