//! What we know about a VOD, and links into it (pipeline.VodInfo, vod_link, timestamp_from_url).

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::LazyLock,
};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::timefmt::{format_hms_tag, parse_ts};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VodInfo {
    pub url: String,
    pub id: String,
    pub title: String,
    pub channel: String,
    pub duration: f64,
    /// quality label ("Source", "720p", "480p", "Audio") -> stream URL
    pub streams: BTreeMap<String, String>,
    pub preview_stream: String,
    pub user_agent: String,
    /// ?t= from the pasted link
    pub url_start: Option<f64>,
    /// unix time the broadcast started (for VOD sync)
    pub start_ts: Option<f64>,
    pub thumbnail: String,
    /// Twitch sprite sheets for hover previews
    pub storyboard: Option<serde_json::Value>,
    /// twitch / youtube / kick / other
    pub site: String,
    /// quality labels whose stream has no sound (YouTube)
    pub video_only: BTreeSet<String>,
}

impl VodInfo {
    /// What the interface gets (VodInfo.to_dict).
    pub fn to_dict(&self) -> serde_json::Value {
        serde_json::json!({
            "url": self.url, "id": self.id, "title": self.title, "channel": self.channel,
            "duration": self.duration, "url_start": self.url_start, "start_ts": self.start_ts,
            "thumbnail": self.thumbnail, "storyboard": self.storyboard, "site": self.site,
            "link": vod_link(self, 0.0),
        })
    }

    /// The best "Source" stream, else 720p, else the preview stream.
    pub fn source_stream(&self) -> &str {
        self.streams.get("Source").or_else(|| self.streams.get("720p")).map(|s| s.as_str()).unwrap_or(&self.preview_stream)
    }
}

/// A link that opens the VOD at `seconds`.
pub fn vod_link(info: &VodInfo, seconds: f64) -> String {
    let s = seconds.max(0.0) as i64;
    if info.site == "twitch" && !info.id.is_empty() {
        return format!("https://www.twitch.tv/videos/{}?t={}", info.id, format_hms_tag(s as f64));
    }
    if info.site == "youtube" && !info.id.is_empty() {
        return format!("https://www.youtube.com/watch?v={}&t={}s", info.id, s);
    }
    info.url.clone()
}

/// Parts of a URL, like Python's urlparse (scheme://netloc/path?query#fragment).
pub struct Url<'a> {
    pub netloc: &'a str,
    pub path: &'a str,
    pub query: &'a str,
}

pub fn urlparse(url: &str) -> Url<'_> {
    let rest = url.split_once('#').map(|x| x.0).unwrap_or(url);
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let rest = match rest.find("://") {
        Some(i) if rest[..i].chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) => &rest[i + 1..],
        _ => rest,
    };
    let (netloc, path) = match rest.strip_prefix("//") {
        Some(r) => match r.find('/') {
            Some(i) => (&r[..i], &r[i..]),
            None => (r, ""),
        },
        None => ("", rest),
    };
    Url { netloc, path, query }
}

fn unquote_plus(s: &str) -> String {
    let b = s.replace('+', " ");
    let bytes = b.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 3 <= bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = hex {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// First non-empty value of a query parameter.
pub fn query_param(url: &str, key: &str) -> Option<String> {
    urlparse(url).query.split(['&', ';']).find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (unquote_plus(k) == key && !v.is_empty()).then(|| unquote_plus(v))
    })
}

/// ?t= from a pasted link, in seconds.
pub fn timestamp_from_url(url: &str) -> Option<f64> {
    query_param(url, "t").and_then(|t| parse_ts(&t).ok())
}

pub static KICK_VOD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)kick\.com/(?:[\w-]+/)?videos?/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})").unwrap()
});

static UNSAFE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\w.-]+").unwrap());

/// A piece of a file name: unsafe characters become "_", at most 40 characters.
pub fn safe_name(name: &str) -> String {
    let s = UNSAFE.replace_all(name, "_");
    let s: String = s.trim_matches('_').chars().take(40).collect();
    if s.is_empty() {
        "stream".into()
    } else {
        s
    }
}

// ---------------------------------------------------------------- local video files

/// A video file on this PC, from a pasted/dropped path ("C:\clips\x.mp4", with or without quotes, or a
/// file:/// link). Only paths that exist; see `looks_like_path` for the error case.
pub fn local_video(text: &str) -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(local_text(text));
    (p.is_absolute() && p.is_file()).then_some(p)
}

/// Text that is meant as a file path (drive letter, network share or file:// link), existing or not.
pub fn looks_like_path(text: &str) -> bool {
    let t = local_text(text);
    let b = t.as_bytes();
    (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')) || t.starts_with("\\\\")
}

fn local_text(text: &str) -> String {
    let t = text.trim().trim_matches('"').trim();
    match t.strip_prefix("file:///") {
        Some(rest) => unquote_plus(&rest.replace('+', "%2B")).replace('/', "\\"),
        None => t.to_string(),
    }
}

/// A short id for a local file that changes when the file does (path, size and modified time), so
/// re-running a section reuses its cut audio/video only while the file is the same.
pub fn local_id(path: &std::path::Path) -> String {
    let meta = std::fs::metadata(path).ok();
    let mtime = meta.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    let key = format!("{}|{}|{mtime}", path.to_string_lossy().to_lowercase(), meta.map(|m| m.len()).unwrap_or(0));
    // FNV-1a: stable across Rust versions, unlike DefaultHasher
    let h = key.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stem: String = safe_name(&stem).chars().take(28).collect();
    format!("{stem}_{:06x}", h & 0xff_ffff)
}
