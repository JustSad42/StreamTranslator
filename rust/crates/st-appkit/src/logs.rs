//! Logging, crash capture and "Report a problem" (logs.py).
//!
//! - logs/app.log: what the app did, with timestamps. 2 MB, plus app.log.1 and app.log.2.
//! - logs/crash.log: panics (Rust's equivalent of a crash), with a backtrace.
//! - running.flag exists while the app runs; if it's still there at the next start, the app didn't close
//!   normally and the interface offers to report it.

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use serde_json::{json, Value};
use st_core::paths::local_dir;

pub const ISSUES: &str = "https://github.com/JustSad42/StreamTranslator/issues/new";
const MAX_BYTES: u64 = 2_000_000;

pub fn log_dir() -> PathBuf {
    local_dir().join("logs")
}

struct Logger {
    dir: PathBuf,
}

static LOGGER: OnceLock<Mutex<Logger>> = OnceLock::new();
static PREVIOUS_CRASH: OnceLock<Option<Value>> = OnceLock::new();

/// Windows version like Python's platform.version() ("10.0.26300").
pub fn windows_version() -> String {
    static V: OnceLock<String> = OnceLock::new();
    V.get_or_init(|| {
        let out = st_media::tools::command(Path::new("cmd")).args(["/c", "ver"]).output().ok();
        let text = out.map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
        regex::Regex::new(r"(\d+\.\d+\.\d+)").unwrap().captures(&text).map(|c| c[1].to_string()).unwrap_or_else(|| "unknown".into())
    })
    .clone()
}

fn write_line(level: &str, msg: &str) {
    let Some(l) = LOGGER.get() else { return };
    let l = l.lock().unwrap();
    let path = l.dir.join("app.log");
    if std::fs::metadata(&path).map(|m| m.len() > MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::remove_file(l.dir.join("app.log.2"));
        let _ = std::fs::rename(l.dir.join("app.log.1"), l.dir.join("app.log.2"));
        let _ = std::fs::rename(&path, l.dir.join("app.log.1"));
    }
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("worker").to_string();
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
        for line in msg.lines() {
            let _ = write!(f, "{ts} {level:<7} {name}: {line}\r\n");
        }
    }
}

pub fn info(msg: &str) {
    write_line("INFO", msg);
}
pub fn warning(msg: &str) {
    write_line("WARNING", msg);
}
pub fn error(msg: &str) {
    write_line("ERROR", msg);
}

/// Start logging, note an unclean previous exit, write the running flag, catch panics.
pub fn setup() {
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = LOGGER.set(Mutex::new(Logger { dir: dir.clone() }));
    let flag = dir.join("running.flag");
    let previous = if flag.exists() {
        let started = std::fs::read_to_string(&flag).unwrap_or_default().trim().to_string();
        warning(&format!("The previous session didn't close normally (started {started})."));
        Some(json!({"started": started, "native": recent_native_crash()}))
    } else {
        None
    };
    let _ = PREVIOUS_CRASH.set(previous);
    let _ = std::fs::write(&flag, format!("{} {}", crate::VERSION, chrono::Local::now().format("%Y-%m-%d %H:%M:%S")));
    info(&format!("StreamTranslator {} started on Windows {}", crate::VERSION, windows_version()));
    let crash = dir.join("crash.log");
    std::panic::set_hook(Box::new(move |p| {
        let bt = std::backtrace::Backtrace::force_capture();
        let thread = std::thread::current().name().unwrap_or("worker").to_string();
        let text = format!("Fatal panic in thread {thread}: {p}\n{bt}");
        write_line("CRITICAL", &text);
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&crash) {
            let _ = writeln!(f, "{} {text}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        }
    }));
}

/// How the last session ended, if not normally: {"started": ..., "native": bool}.
pub fn previous_crash() -> Option<Value> {
    PREVIOUS_CRASH.get().cloned().flatten()
}

pub fn clean_exit() {
    info("Closed normally.");
    let _ = std::fs::remove_file(log_dir().join("running.flag"));
}

fn recent_native_crash() -> bool {
    let p = log_dir().join("crash.log");
    let Ok(m) = std::fs::metadata(&p) else { return false };
    let recent = m.modified().ok().and_then(|t| t.elapsed().ok()).is_some_and(|e| e.as_secs() < 7 * 86400);
    m.len() > 0 && recent && tail(&p, 40).to_lowercase().contains("fatal")
}

pub fn tail(path: &Path, lines: usize) -> String {
    let Ok(b) = std::fs::read(path) else { return String::new() };
    let b = &b[b.len().saturating_sub(200_000)..];
    let text = String::from_utf8_lossy(b);
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// Hide the Windows user name in paths.
pub fn redact(text: &str) -> String {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let mut t = text.to_string();
    if !home.is_empty() {
        t = t.replace(&home, "~").replace(&home.replace('\\', "/"), "~");
    }
    regex::Regex::new(r"(?i)[A-Z]:\\Users\\[^\\\s]+").unwrap().replace_all(&t, "~").into_owned()
}

/// urllib.parse.quote_plus
fn quote_plus(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A pre-filled GitHub issue. Nothing is sent: it opens in the browser for the user to read and submit.
/// `app_log`/`crash_log` are parameters so tests can point them elsewhere.
pub fn report_url_with(what: &str, gpu: &str, extra: &[(&str, String)], app_log: &Path, crash_log: Option<&Path>) -> String {
    let mut info = vec![
        format!("- Version: {}", crate::VERSION),
        format!("- Windows: {}", windows_version()),
        format!("- GPU: {}", if gpu.is_empty() { "unknown" } else { gpu }),
    ];
    for (k, v) in extra {
        info.push(format!("- {k}: {v}"));
    }
    let crash = crash_log.map(|p| tail(p, 25)).unwrap_or_default();
    let body = |n: usize| {
        let mut parts: Vec<String> = vec![
            "**What happened?**".into(),
            if what.is_empty() { "(describe what you were doing)".into() } else { what.into() },
            String::new(),
            "**Details**".into(),
        ];
        parts.extend(info.clone());
        parts.extend(["".into(), "<details><summary>Recent log</summary>".into(), "".into(), "```".into(), redact(&tail(app_log, n)), "```".into(), "</details>".into()]);
        if !crash.is_empty() {
            parts.extend(["".into(), "<details><summary>Crash log</summary>".into(), "".into(), "```".into(), redact(&crash), "```".into(), "</details>".into()]);
        }
        parts.join("\n")
    };
    let first = if what.is_empty() { "Problem" } else { what.lines().next().unwrap_or("Problem") };
    let title = format!("[{}] {}", crate::VERSION, first.chars().take(80).collect::<String>());
    // browsers and GitHub cut very long links: shrink the log until it fits
    for n in [80, 50, 30, 15, 5] {
        let url = format!("{ISSUES}?title={}&body={}", quote_plus(&title), quote_plus(&body(n)));
        if url.len() < 7500 {
            return url;
        }
    }
    format!("{ISSUES}?title={}&body={}", quote_plus(&title), quote_plus(&body(0).chars().take(3000).collect::<String>()))
}

pub fn report_url(what: &str, gpu: &str, extra: &[(&str, String)]) -> String {
    let crash = log_dir().join("crash.log");
    report_url_with(what, gpu, extra, &log_dir().join("app.log"), recent_native_crash().then_some(crash.as_path()))
}
