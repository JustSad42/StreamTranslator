//! VOD info via the official yt-dlp.exe (`-J`), with the Kick API fallback (pipeline.fetch_info,
//! sync.list_vods).

use std::{process::Stdio, sync::LazyLock};

use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use st_core::{
    job_err,
    sync::{VodEntry, START_IN_THUMB},
    vod::{timestamp_from_url, VodInfo, KICK_VOD},
};

use crate::tools::{command, ytdlp_exe};

static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*m").unwrap());

/// Run yt-dlp and parse its JSON. Err(message) carries yt-dlp's own error text.
fn ytdlp_json(args: &[&str], url: &str) -> std::result::Result<Value, String> {
    let exe = ytdlp_exe().map_err(|e| e.0)?;
    let out = command(&exe)
        .args(["-J", "--no-warnings", "--no-progress", "--encoding", "utf-8"])
        .args(args)
        .arg("--")
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't run yt-dlp: {e}"))?;
    if !out.status.success() || out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = ANSI.replace_all(err.trim(), "").replace("ERROR: ", "");
        return Err(if err.is_empty() { "yt-dlp failed".into() } else { err });
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("yt-dlp gave unreadable output: {e}"))
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}
fn num(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)
}
/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}
fn codec_none(v: &Value, k: &str) -> bool {
    matches!(v.get(k), None | Some(Value::Null)) || s(v, k) == "none"
}

/// Seconds since 1970 for an ISO-8601 time ("2025-03-01T18:04:05.123Z", "+01:00" offsets ok).
pub fn iso_to_unix(t: &str) -> Option<f64> {
    let re = Regex::new(r"^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2}(?:\.\d+)?)(Z|[+-]\d{2}:?\d{2})?$").ok()?;
    let c = re.captures(t.trim())?;
    let n = |i: usize| c[i].parse::<i64>().unwrap();
    let (y, m, d) = (n(1), n(2), n(3));
    // days from civil (Howard Hinnant)
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let mut secs = days as f64 * 86400.0 + n(4) as f64 * 3600.0 + n(5) as f64 * 60.0 + c[6].parse::<f64>().unwrap();
    if let Some(tz) = c.get(7).map(|m| m.as_str()).filter(|z| *z != "Z") {
        let digits: String = tz[1..].chars().filter(|c| c.is_ascii_digit()).collect();
        let off = digits[..2].parse::<f64>().unwrap() * 3600.0 + digits[2..].parse::<f64>().unwrap() * 60.0;
        secs -= if tz.starts_with('+') { off } else { -off };
    }
    Some(secs)
}

/// yt-dlp's Kick reader breaks whenever Kick changes its API; Kick's own video API still gives the
/// stream playlist, which yt-dlp reads fine.
fn kick_info(url: &str) -> Option<Value> {
    let id = KICK_VOD.captures(url)?.get(1)?.as_str().to_string();
    let d: Value = ureq::get(&format!("https://kick.com/api/v1/video/{id}"))
        .header("User-Agent", "Mozilla/5.0")
        .header("Accept", "application/json")
        .call()
        .ok()?
        .body_mut()
        .read_json()
        .ok()?;
    let mut info = ytdlp_json(&[], s(&d, "source")).ok()?;
    let ls = d.get("livestream").cloned().unwrap_or(Value::Null);
    let start = Some(s(&ls, "start_time")).filter(|x| !x.is_empty()).unwrap_or(s(&d, "created_at"));
    let o = info.as_object_mut()?;
    o.insert("extractor_key".into(), "Kick".into());
    o.insert("id".into(), id.into());
    o.insert("webpage_url".into(), url.into());
    o.insert("title".into(), Some(s(&ls, "session_title")).filter(|x| !x.is_empty()).unwrap_or("Kick VOD").into());
    let slug = ls.get("channel").map(|c| s(c, "slug")).filter(|x| !x.is_empty()).unwrap_or("kick");
    o.insert("uploader".into(), slug.into());
    o.insert("thumbnail".into(), s(&ls, "thumbnail").into());
    o.insert("timestamp".into(), iso_to_unix(start).map(Value::from).unwrap_or(Value::Null));
    if !truthy(o.get("duration")) {
        o.insert("duration".into(), (num(&ls, "duration") / 1000.0).into());
    }
    Some(info)
}

/// Read a VOD's title, length and stream URLs. Always re-read before a job (Twitch tokens expire).
pub fn fetch_info(url: &str) -> Result<VodInfo> {
    let mut url = url.trim().to_string();
    if url.is_empty() {
        return job_err("Paste a VOD link first (Twitch, YouTube or Kick).");
    }
    if url.chars().all(|c| c.is_ascii_digit()) {
        url = format!("https://www.twitch.tv/videos/{url}");
    }
    let info = match ytdlp_json(&["--flat-playlist"], &url) {
        Ok(i) => i,
        Err(msg) => match kick_info(&url) {
            Some(i) => i,
            None => {
                let low = url.to_lowercase();
                let hint = if low.contains("twitch") {
                    "\n\nSubscriber-only VODs can't be downloaded without logging in."
                } else if low.contains("youtu") {
                    "\n\nMembers-only, private and age-restricted videos can't be downloaded."
                } else {
                    ""
                };
                return job_err(format!("Couldn't read that VOD.\n{msg}{hint}"));
            }
        },
    };
    if s(&info, "_type") == "playlist" {
        return job_err("That link is a channel or playlist. Paste a single VOD link (twitch.tv/videos/123…, a YouTube video, or a Kick video).");
    }
    if truthy(info.get("is_live")) {
        return job_err("That stream is live. Use the Live tab, or wait for the VOD.");
    }
    vod_from_json(&info, &url)
}

/// The VodInfo for yt-dlp's JSON (separate so it can be tested without the network).
pub fn vod_from_json(info: &Value, url: &str) -> Result<VodInfo> {
    let key = s(info, "extractor_key").to_lowercase();
    let site = if key.contains("twitch") {
        "twitch"
    } else if key.contains("youtube") {
        "youtube"
    } else if key.contains("kick") {
        "kick"
    } else {
        "other"
    };
    let has_audio = |f: &Value| !codec_none(f, "acodec") || (matches!(f.get("acodec"), None | Some(Value::Null)) && site != "youtube");
    let empty = vec![];
    let formats = info.get("formats").and_then(|f| f.as_array()).unwrap_or(&empty);
    let mut vids: Vec<&Value> = formats.iter().filter(|f| truthy(f.get("url")) && !codec_none(f, "vcodec") && truthy(f.get("height"))).collect();
    if vids.is_empty() {
        return job_err("No playable video formats found for that link.");
    }
    // at equal height prefer streams with sound, then H.264 (plays everywhere, copies cleanly into .mp4)
    let vkey = |f: &Value| (num(f, "height"), has_audio(f), s(f, "vcodec").starts_with("avc"), num(f, "fps"), num(f, "tbr"));
    vids.sort_by(|a, b| vkey(a).partial_cmp(&vkey(b)).unwrap_or(std::cmp::Ordering::Equal));
    let best_under = |h: f64| -> &Value {
        let under: Vec<&&Value> = vids.iter().filter(|f| num(f, "height") <= h).collect();
        under.last().map(|f| **f).unwrap_or(vids[0])
    };
    let picks = [("Source", *vids.last().unwrap()), ("720p", best_under(720.0)), ("480p", best_under(480.0))];
    let mut info_out = VodInfo { site: site.into(), ..Default::default() };
    for (k, f) in &picks {
        info_out.streams.insert(k.to_string(), s(f, "url").to_string());
    }
    let mut auds: Vec<&Value> = formats
        .iter()
        .filter(|f| truthy(f.get("url")) && codec_none(f, "vcodec") && !codec_none(f, "acodec") && s(f, "format_note") != "storyboard")
        .collect();
    // AAC first (fits .mp4/.m4a as is), skip YouTube's "DRC" loudness-squashed copies, then best bitrate
    let rate = |f: &Value| if num(f, "abr") != 0.0 { num(f, "abr") } else { num(f, "tbr") };
    let akey = |f: &Value| (!s(f, "format_id").to_lowercase().contains("drc"), s(f, "acodec").starts_with("mp4a"), rate(f));
    auds.sort_by(|a, b| akey(a).partial_cmp(&akey(b)).unwrap_or(std::cmp::Ordering::Equal));
    if let Some(a) = auds.last() {
        info_out.streams.insert("Audio".into(), s(a, "url").to_string());
    }
    info_out.video_only = picks.iter().filter(|(_, f)| !has_audio(f)).map(|(k, _)| k.to_string()).collect();
    if !info_out.video_only.is_empty() && !info_out.streams.contains_key("Audio") {
        return job_err("That video has no downloadable sound track.");
    }
    info_out.preview_stream = s(vids.iter().find(|f| num(f, "height") >= 360.0).unwrap_or(vids.last().unwrap()), "url").to_string();
    info_out.user_agent = vids.last().unwrap().get("http_headers").map(|h| s(h, "User-Agent").to_string()).unwrap_or_default();
    let sbs: Vec<&Value> = formats.iter().filter(|f| s(f, "format_note") == "storyboard" && truthy(f.get("fragments"))).collect();
    if let Some(sb) = sbs.iter().copied().fold(None::<&Value>, |best, f| match best {
        Some(b) if num(b, "width") >= num(f, "width") => Some(b),
        _ => Some(f),
    }) {
        if truthy(sb.get("fps")) {
            let sheets: Vec<Value> = sb["fragments"].as_array().unwrap().iter().map(|fr| fr["url"].clone()).collect();
            let or = |k: &str, d: f64| if truthy(sb.get(k)) { num(sb, k) } else { d };
            info_out.storyboard = Some(serde_json::json!({
                "sheets": sheets, "cols": or("columns", 5.0) as i64, "rows": or("rows", 10.0) as i64,
                "w": sb.get("width"), "h": sb.get("height"), "interval": 1.0 / num(sb, "fps"),
            }));
        }
    }
    info_out.start_ts = truthy(info.get("timestamp")).then(|| num(info, "timestamp"));
    info_out.thumbnail = s(info, "thumbnail").replace("0x0", "640x360");
    info_out.url = Some(s(info, "webpage_url")).filter(|x| !x.is_empty()).unwrap_or(url).to_string();
    let id = match info.get("id") {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    info_out.id = if site == "twitch" { id.trim_start_matches('v').to_string() } else { id };
    let first = |keys: &[&str], d: &str| keys.iter().map(|k| s(info, k)).find(|v| !v.is_empty()).unwrap_or(d).to_string();
    info_out.title = first(&["title", "description"], "Untitled");
    info_out.channel = first(&["uploader", "uploader_id", "channel"], "streamer");
    info_out.duration = num(info, "duration");
    info_out.url_start = timestamp_from_url(url);
    Ok(info_out)
}

/// Recent past broadcasts for a Twitch channel, newest first.
pub fn list_vods(login: &str, limit: usize) -> Result<Vec<VodEntry>> {
    let end = limit.to_string();
    let p = match ytdlp_json(&["--flat-playlist", "--playlist-end", &end], &format!("https://www.twitch.tv/{login}/videos?filter=archives&sort=time")) {
        Ok(p) => p,
        Err(msg) => return job_err(format!("Couldn't list {login}'s VODs.\n{msg}")),
    };
    let empty = vec![];
    let out: Vec<VodEntry> = p
        .get("entries")
        .and_then(|e| e.as_array())
        .unwrap_or(&empty)
        .iter()
        .map(|e| {
            let thumb = s(e, "thumbnail");
            let id = match e.get("id") {
                Some(Value::String(x)) => x.trim_start_matches('v').to_string(),
                Some(Value::Number(n)) => n.to_string(),
                _ => String::new(),
            };
            VodEntry {
                url: Some(s(e, "url")).filter(|x| !x.is_empty()).map(String::from).unwrap_or(format!("https://www.twitch.tv/videos/{id}")),
                id,
                title: Some(s(e, "title")).filter(|x| !x.is_empty()).unwrap_or("Untitled").into(),
                duration: num(e, "duration"),
                thumbnail: if thumb.contains("404_processing") { String::new() } else { thumb.into() },
                start_ts: START_IN_THUMB.captures(thumb).map(|c| c[1].parse().unwrap()),
                views: e.get("view_count").and_then(|v| v.as_u64()),
                channel: login.into(),
            }
        })
        .collect();
    if out.is_empty() {
        return job_err(format!("{login} has no saved past broadcasts (or they're sub-only)."));
    }
    Ok(out)
}

/// A live stream's audio and preview URLs (live.py resolve_live).
#[derive(Clone, Debug, Default)]
pub struct LiveInfo {
    pub url: String,
    pub audio: String,
    pub preview: Option<String>,
    pub ua: String,
    pub title: String,
    pub channel: String,
    pub thumbnail: String,
}

pub fn resolve_live(text: &str) -> Result<LiveInfo> {
    use st_core::sync::{classify, Kind};
    let (kind, value) = classify(text)?;
    if kind == Kind::Vod {
        return job_err("That's a VOD link. Use the Clip tab for VODs. Live mode needs a channel that's live now.");
    }
    let url = if kind == Kind::Channel { format!("https://www.twitch.tv/{value}") } else { value.clone() };
    let info = match ytdlp_json(&[], &url) {
        Ok(i) => i,
        Err(msg) => {
            if msg.contains("not currently live") || msg.to_lowercase().contains("offline") {
                return job_err(format!("{value} isn't live right now."));
            }
            return job_err(format!("Couldn't open that stream.\n{msg}"));
        }
    };
    if s(&info, "_type") == "playlist" {
        return job_err("That link is a playlist, not a live stream.");
    }
    if !truthy(info.get("is_live")) {
        return job_err("That stream isn't live right now.");
    }
    let fmts: Vec<&Value> = info.get("formats").and_then(|f| f.as_array()).map(|a| a.iter().filter(|f| !s(f, "url").is_empty()).collect()).unwrap_or_default();
    let audio: Vec<&Value> = fmts.iter().copied().filter(|f| codec_none(f, "vcodec") && !codec_none(f, "acodec") && s(f, "format_note") != "storyboard").collect();
    let mut video: Vec<&Value> = fmts.iter().copied().filter(|f| !codec_none(f, "vcodec") && truthy(f.get("height"))).collect();
    video.sort_by(|a, b| num(*a, "height").total_cmp(&num(*b, "height")));
    if audio.is_empty() && video.is_empty() {
        return job_err("No playable formats found for that stream.");
    }
    let audio_url = s(audio.first().or(video.first()).unwrap(), "url").to_string();
    let preview = video.iter().copied().find(|f| num(*f, "height") >= 160.0).or(video.first().copied()).map(|f| s(f, "url").to_string());
    let ua = video.first().or(audio.first()).and_then(|f| f.get("http_headers")).map(|h| s(h, "User-Agent").to_string()).unwrap_or_default();
    let first = |keys: &[&str]| keys.iter().map(|k| s(&info, k)).find(|v| !v.is_empty()).unwrap_or("").to_string();
    let channel = Some(first(&["uploader", "uploader_id", "channel"])).filter(|c| !c.is_empty()).unwrap_or(value);
    Ok(LiveInfo { url, audio: audio_url, preview, ua, title: first(&["description", "title"]), channel, thumbnail: s(&info, "thumbnail").to_string() })
}
