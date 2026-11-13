//! Line up two streams by wall-clock time (sync.py). The network parts (listing a channel's VODs,
//! reading their VOD) live in st-media; this is the matching logic.

use std::sync::LazyLock;

use regex::Regex;

use crate::{vod::urlparse, vod::VodInfo, JobError};

static LOGIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_]{3,25}$").unwrap());
const RESERVED: &[&str] = &["videos", "directory", "settings", "search", "p", "downloads", "jobs", "turbo"];
pub static START_IN_THUMB: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"_(\d{10})/+(?:thumb|storyboards)").unwrap());

#[derive(Debug, PartialEq, Eq)]
pub enum Kind {
    Vod,
    Channel,
    Url,
}

/// (Vod, url) | (Channel, login) | (Url, url) for anything else yt-dlp might read.
pub fn classify(text: &str) -> Result<(Kind, String), JobError> {
    let s = text.trim();
    if s.is_empty() {
        return Err(JobError("Paste a link or a channel name.".into()));
    }
    if let Some(file) = crate::vod::local_video(s) {
        return Ok((Kind::Vod, file.to_string_lossy().into_owned()));
    }
    if crate::vod::looks_like_path(s) {
        return Err(JobError(format!("Couldn't find that file:\n{}", s.trim_matches('"'))));
    }
    if s.chars().all(|c| c.is_ascii_digit()) {
        return Ok((Kind::Vod, format!("https://www.twitch.tv/videos/{s}")));
    }
    if LOGIN.is_match(s) {
        return Ok((Kind::Channel, s.to_lowercase()));
    }
    let s = if s.starts_with("http") { s.to_string() } else { format!("https://{s}") };
    let u = urlparse(&s);
    let host = u.netloc.to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let host = host.strip_prefix("m.").unwrap_or(host);
    let parts: Vec<&str> = u.path.split('/').filter(|p| !p.is_empty()).collect();
    if host.ends_with("twitch.tv") {
        if parts.len() >= 2 && parts[0] == "videos" && parts[1].chars().all(|c| c.is_ascii_digit()) && !parts[1].is_empty() {
            return Ok((Kind::Vod, s));
        }
        if let Some(p) = parts.first() {
            if !RESERVED.contains(&p.to_lowercase().as_str()) && LOGIN.is_match(p) {
                return Ok((Kind::Channel, p.to_lowercase()));
            }
        }
    }
    Ok((Kind::Url, s))
}

/// One of a channel's past broadcasts (from the channel's VOD list).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct VodEntry {
    pub id: String,
    pub url: String,
    pub title: String,
    pub duration: f64,
    pub thumbnail: String,
    pub start_ts: Option<f64>,
    pub views: Option<u64>,
    pub channel: String,
}

/// The VOD that was live at `moment` (unix time), with a minute of slack either side.
pub fn vod_covering(vods: &[VodEntry], moment: f64) -> Option<&VodEntry> {
    vods.iter().find(|v| v.start_ts.is_some_and(|s| s - 60.0 <= moment && moment <= s + v.duration + 60.0))
}

#[derive(Debug)]
pub struct Matched {
    pub start: f64,
    pub end: f64,
    /// add to my time to get theirs
    pub offset: f64,
    /// part of the section is outside their VOD
    pub clipped: bool,
}

/// Map my section onto their VOD.
pub fn map_times(mine: &VodInfo, my_start: f64, my_end: f64, theirs: &VodInfo) -> Result<Matched, JobError> {
    let (Some(a), Some(b)) = (mine.start_ts, theirs.start_ts) else {
        return Err(JobError("Twitch didn't say when the VOD started, so it can't be synced.".into()));
    };
    let offset = a - b;
    let (start, end) = (my_start + offset, my_end + offset);
    if end < 0.0 || start > theirs.duration {
        return Err(JobError(
            "Their VOD doesn't cover that moment. They may not have been live yet, or the VOD was trimmed.".into(),
        ));
    }
    Ok(Matched { start: start.max(0.0), end: end.min(theirs.duration), offset, clipped: start < 0.0 || end > theirs.duration })
}
