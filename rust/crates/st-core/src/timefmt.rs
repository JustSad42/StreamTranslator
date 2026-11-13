//! Timestamp parsing and formatting (timefmt.py).

use std::sync::LazyLock;

use regex::Regex;

static HMS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^\s*(?:(\d+)h)?\s*(?:(\d+)m)?\s*(?:(\d+(?:\.\d+)?)s)?\s*$").unwrap());

fn py_float(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.parse::<f64>().ok()
}

/// Accepts "1:02:30", "62:30", "3750", "1h2m30s", "45m".
pub fn parse_ts(text: &str) -> Result<f64, String> {
    let s = text.trim();
    if s.is_empty() {
        return Err("empty timestamp".into());
    }
    let bad = || format!("bad timestamp: {text:?}");
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() > 3 || parts.iter().any(|p| p.trim().is_empty()) {
            return Err(bad());
        }
        let mut total = 0.0;
        for p in parts {
            total = total * 60.0 + py_float(p).ok_or_else(bad)?;
        }
        return Ok(total);
    }
    if let Some(v) = py_float(s) {
        return Ok(v);
    }
    if let Some(c) = HMS.captures(s) {
        if c.get(1).is_some() || c.get(2).is_some() || c.get(3).is_some() {
            let n = |i: usize| c.get(i).map(|m| m.as_str().parse::<f64>().unwrap()).unwrap_or(0.0);
            return Ok(n(1) * 3600.0 + n(2) * 60.0 + n(3));
        }
    }
    Err(bad())
}

/// 3750 -> "1:02:30", 95 -> "1:35".
pub fn format_ts(seconds: f64) -> String {
    let s = seconds.max(0.0).round_ties_even() as i64;
    let (h, m, sec) = (s / 3600, s % 3600 / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// 3750 -> "1h02m30s" (file names and Twitch ?t= links).
pub fn format_hms_tag(seconds: f64) -> String {
    let s = seconds.max(0.0) as i64;
    format!("{}h{:02}m{:02}s", s / 3600, s % 3600 / 60, s % 60)
}

pub fn srt_ts(seconds: f64) -> String {
    let ms = (seconds.max(0.0) * 1000.0).round_ties_even() as i64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms % 3_600_000 / 60_000, ms % 60_000 / 1000, ms % 1000)
}
