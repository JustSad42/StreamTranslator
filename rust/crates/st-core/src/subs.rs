//! Subtitle and transcript files (pipeline.py: to_cues, write_srt, read_srt, write_srt_cues,
//! write_transcript; translate.py: split_sentences).

use std::{path::Path, sync::LazyLock};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{
    cluster::color,
    langs::lang_name,
    py,
    timefmt::{format_ts, srt_ts},
    vod::{vod_link, VodInfo},
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    /// 0 = the person who talks most; others get coloured subtitles
    #[serde(default)]
    pub speaker: usize,
}

impl Segment {
    pub fn new(start: f64, end: f64, text: impl Into<String>) -> Self {
        Segment { start, end, text: text.into(), speaker: 0 }
    }
}

fn clen(s: &str) -> usize {
    s.chars().count()
}

fn char_slice(s: &str, a: usize, b: usize) -> String {
    s.chars().skip(a).take(b.saturating_sub(a)).collect()
}

pub fn is_cjk(text: &str) -> bool {
    text.chars().filter(|&c| c as u32 > 0x2E80).count() as f64 > clen(text) as f64 * 0.3
}

fn chunks(text: &str, limit: usize) -> Vec<String> {
    let n = clen(text);
    if n <= limit {
        return vec![text.to_string()];
    }
    if is_cjk(text) {
        return (0..n).step_by(limit).map(|i| char_slice(text, i, i + limit)).collect();
    }
    let mut out = vec![];
    let mut cur = String::new();
    for w in text.split_whitespace() {
        if !cur.is_empty() && clen(&cur) + 1 + clen(w) > limit {
            out.push(std::mem::take(&mut cur));
            cur = w.to_string();
        } else {
            cur = format!("{cur} {w}").trim().to_string();
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Two lines of similar length instead of one full line plus a stray word.
pub fn balanced_wrap(text: &str, line_len: usize) -> String {
    let n = clen(text);
    if n <= line_len {
        return text.to_string();
    }
    for width in (n / 2).max(12)..=line_len {
        let lines = py::wrap(text, width);
        if lines.len() <= 2 {
            return lines.join("\n");
        }
    }
    py::wrap(text, line_len).into_iter().take(2).collect::<Vec<_>>().join("\n")
}

/// Split long Whisper segments into readable 2-line cues with proportional timing.
pub fn to_cues(segments: &[Segment]) -> Vec<Segment> {
    let mut cues: Vec<Segment> = vec![];
    for seg in segments {
        let cjk = is_cjk(&seg.text);
        let line_len = if cjk { 22 } else { 42 };
        let parts = chunks(&seg.text, line_len * 2);
        let total = parts.iter().map(|p| clen(p)).sum::<usize>().max(1) as f64;
        let mut t = seg.start;
        for p in parts {
            let d = (seg.end - seg.start) * clen(&p) as f64 / total;
            let wrapped = if cjk {
                if clen(&p) > line_len {
                    format!("{}\n{}", char_slice(&p, 0, line_len), char_slice(&p, line_len, usize::MAX))
                } else {
                    p.clone()
                }
            } else {
                balanced_wrap(&p, line_len)
            };
            cues.push(Segment { start: t, end: t + d, text: wrapped, speaker: seg.speaker });
            t += d;
        }
    }
    // minimum display time without overlapping the next cue
    for i in 0..cues.len() {
        let next = if i + 1 < cues.len() { cues[i + 1].start } else { cues[i].end + 2.0 };
        let c = &mut cues[i];
        if c.end - c.start < 1.2 {
            c.end = (c.start + 1.2).min(c.end.max(next - 0.05));
        }
    }
    cues
}

fn colored(text: &str, speaker: usize) -> String {
    match color(speaker) {
        Some(c) => format!("<font color=\"{c}\">{text}</font>"),
        None => text.to_string(),
    }
}

/// `shift`: seconds added to every time (to line subtitles up with a video that starts earlier).
pub fn write_srt(segments: &[Segment], path: &Path, shift: f64) -> std::io::Result<()> {
    let mut lines: Vec<String> = vec![];
    for (i, c) in to_cues(segments).iter().enumerate() {
        lines.push((i + 1).to_string());
        lines.push(format!("{} --> {}", srt_ts((c.start + shift).max(0.0)), srt_ts((c.end + shift).max(0.0))));
        lines.push(colored(&c.text, c.speaker));
        lines.push(String::new());
    }
    py::write_text(path, &lines.join("\n"))
}

/// A subtitle as the editor sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

static BLOCK_SEP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\n\s*\n").unwrap());
static TIMES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(\d+):(\d+):(\d+)[,.](\d+)\s*-->\s*(\d+):(\d+):(\d+)[,.](\d+)").unwrap());
static FONT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?s)^<font color="(#[0-9A-Fa-f]{6})">(.*)</font>$"#).unwrap());

pub fn read_srt(path: &Path) -> std::io::Result<Vec<Cue>> {
    let raw = std::fs::read_to_string(path)?;
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw).replace("\r\n", "\n");
    let mut cues = vec![];
    for b in BLOCK_SEP.split(raw.trim()) {
        let lines: Vec<&str> = b.split('\n').collect();
        for (i, ln) in lines.iter().take(2).enumerate() {
            if let Some(m) = TIMES.captures(ln) {
                let g: Vec<f64> = (1..=8).map(|k| m[k].parse::<f64>().unwrap()).collect();
                let text = lines[i + 1..].join("\n").trim().to_string();
                let (text, color) = match FONT.captures(&text) {
                    Some(f) => (f[2].trim().to_string(), Some(f[1].to_string())),
                    None => (text, None),
                };
                cues.push(Cue {
                    start: g[0] * 3600.0 + g[1] * 60.0 + g[2] + g[3] / 1000.0,
                    end: g[4] * 3600.0 + g[5] * 60.0 + g[6] + g[7] / 1000.0,
                    text,
                    color,
                });
                break;
            }
        }
    }
    Ok(cues)
}

/// Write cues exactly as edited (no re-splitting): sorted, blank ones dropped.
pub fn write_srt_cues(cues: &[Cue], path: &Path) -> std::io::Result<()> {
    let mut keep: Vec<&Cue> = cues.iter().filter(|c| !c.text.trim().is_empty()).collect();
    keep.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<String> = vec![];
    for (i, c) in keep.iter().enumerate() {
        let text = c.text.trim();
        let text = match &c.color {
            Some(col) if !col.is_empty() => format!("<font color=\"{col}\">{text}</font>"),
            _ => text.to_string(),
        };
        out.push((i + 1).to_string());
        out.push(format!("{} --> {}", srt_ts(c.start), srt_ts(c.end.max(c.start + 0.2))));
        out.push(text);
        out.push(String::new());
    }
    py::write_text(path, &out.join("\n"))
}

/// What goes into the transcript's header.
pub struct TranscriptInfo<'a> {
    pub info: &'a VodInfo,
    pub start: f64,
    pub end: f64,
    pub lang: &'a str,
    pub prob: f64,
    pub model: &'a str,
}

/// `clip_shift`: how much later a moment appears in the saved video than in the VOD section.
pub fn write_transcript(path: &Path, t: &TranscriptInfo, english: &[Segment], original: Option<&[Segment]>, clip_shift: f64) -> std::io::Result<()> {
    let speakers: std::collections::HashSet<usize> = english.iter().map(|s| s.speaker).collect();
    let many = speakers.len() > 1;
    let block = |segs: &[Segment]| -> Vec<String> {
        segs.iter()
            .map(|s| {
                let who = if many { format!("Speaker {}: ", s.speaker + 1) } else { String::new() };
                format!("[VOD {:>8} | clip {:>6}]  {who}{}", format_ts(t.start + s.start), format_ts(s.start + clip_shift), s.text)
            })
            .collect()
    };
    let name = lang_name(Some(t.lang));
    let mut lines = vec![
        t.info.title.clone(),
        format!("Channel: {}", t.info.channel),
        format!("VOD: {}", t.info.url),
        format!("Section: {} - {}   ({})", format_ts(t.start), format_ts(t.end), vod_link(t.info, t.start)),
        format!("Detected language: {name} ({:.0}%)   Model: whisper {}", t.prob * 100.0, t.model),
        String::new(),
    ];
    if english.is_empty() {
        lines.push("(no speech found)".into());
    } else {
        lines.push("=== English ===".into());
        lines.extend(block(english));
    }
    if let Some(orig) = original.filter(|o| !o.is_empty()) {
        lines.push(String::new());
        lines.push(format!("=== Original ({name}) ==="));
        lines.extend(block(orig));
    }
    py::write_text(path, &(lines.join("\n") + "\n"))
}

/// Split text into sentences (NLLB drops later sentences of multi-sentence input).
pub fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut parts = vec![];
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        cur.push(c);
        i += 1;
        let ws_follows = i < chars.len() && chars[i].is_whitespace();
        if ("。！？".contains(c)) || (".!?…".contains(c) && ws_follows) {
            parts.push(std::mem::take(&mut cur));
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
        }
    }
    parts.push(cur);
    let parts: Vec<String> = parts.iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        vec![text.to_string()]
    } else {
        parts
    }
}
