//! Updating the app from its GitHub releases (appupdate.py). Must stay compatible with the installed
//! Python app's updater: same release asset name, Inno Setup installer, same silent flags.

use std::{
    io::{Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command},
};

use anyhow::{bail, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use st_core::{cancel::Cancel, paths::local_dir};

use crate::VERSION;

pub const REPO: &str = "JustSad42/StreamTranslator";
pub const PAGE: &str = "https://github.com/JustSad42/StreamTranslator/releases/latest";

pub fn dir() -> PathBuf {
    local_dir().join("updates")
}

fn vt(v: &str) -> Vec<u64> {
    regex::Regex::new(r"\d+").unwrap().find_iter(if v.is_empty() { "0" } else { v }).take(3).map(|m| m.as_str().parse().unwrap_or(0)).collect()
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    vt(latest) > vt(current)
}

fn get_json(url: &str, timeout_s: u64) -> Result<Value> {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(std::time::Duration::from_secs(timeout_s))).build().into();
    Ok(agent.get(url).header("User-Agent", "StreamTranslator").header("Accept", "application/vnd.github+json").call()?.body_mut().read_json()?)
}

/// {current, latest, newer, notes, url, asset: {name, url, size, sha256}} for a GitHub release.
pub fn parse(rel: &Value) -> Value {
    let latest = rel["tag_name"].as_str().unwrap_or("").trim_start_matches(['v', 'V']).to_string();
    let empty = vec![];
    let asset = rel["assets"].as_array().unwrap_or(&empty).iter().find(|a| {
        let n = a["name"].as_str().unwrap_or("").to_lowercase();
        n.starts_with("streamtranslator-setup") && n.ends_with(".exe")
    });
    let asset_v = asset.map(|a| {
        let digest = a["digest"].as_str().unwrap_or("");
        json!({
            "name": a["name"], "url": a["browser_download_url"], "size": a["size"].as_u64().unwrap_or(0),
            "sha256": digest.strip_prefix("sha256:").map(|d| d.to_lowercase()).unwrap_or_default(),
        })
    });
    json!({
        "current": VERSION, "latest": latest, "notes": rel["body"].as_str().unwrap_or("").trim(),
        "url": rel["html_url"].as_str().filter(|s| !s.is_empty()).unwrap_or(PAGE), "asset": asset_v,
        "newer": asset.is_some() && is_newer(&latest, VERSION),
    })
}

pub fn check() -> Result<Value> {
    Ok(parse(&get_json(&format!("https://api.github.com/repos/{REPO}/releases/latest"), 15)?))
}

/// Same shape as check(), for one version (to go back to the previous one).
pub fn release_info(version: &str) -> Result<Value> {
    Ok(parse(&get_json(&format!("https://api.github.com/repos/{REPO}/releases/tags/v{}", version.trim_start_matches(['v', 'V'])), 15)?))
}

pub fn save_notes(version: &str, notes: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir())?;
    st_core::py::write_text(&dir().join(format!("notes-{version}.md")), notes)
}

/// Release notes: saved when the update was downloaded, else asked from GitHub.
pub fn notes_for(version: &str) -> String {
    if let Ok(s) = std::fs::read_to_string(dir().join(format!("notes-{version}.md"))) {
        return s.replace("\r\n", "\n");
    }
    release_info(version).ok().and_then(|r| r["notes"].as_str().map(String::from)).unwrap_or_default()
}

/// Write the installer from `reader` into `to_dir`, checking size and SHA-256. Nothing is left behind
/// when a check fails.
pub fn save_verified(reader: &mut dyn Read, asset: &Value, to_dir: &Path, progress: &dyn Fn(f64), cancel: Option<&Cancel>) -> Result<PathBuf> {
    std::fs::create_dir_all(to_dir)?;
    let name = Path::new(asset["name"].as_str().unwrap_or("installer.exe")).file_name().unwrap().to_string_lossy().into_owned();
    let target = to_dir.join(&name);
    let part = to_dir.join(format!("{name}.part"));
    if let Ok(rd) = std::fs::read_dir(to_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("StreamTranslator-Setup-")) && p != target {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let size = asset["size"].as_u64().unwrap_or(0);
    let mut h = Sha256::new();
    let mut got = 0u64;
    let mut f = std::fs::File::create(&part)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.is_some_and(|c| c.is_set()) {
            drop(f);
            let _ = std::fs::remove_file(&part);
            bail!("Update cancelled");
        }
        let k = reader.read(&mut buf)?;
        if k == 0 {
            break;
        }
        f.write_all(&buf[..k])?;
        h.update(&buf[..k]);
        got += k as u64;
        if size > 0 {
            progress(got as f64 / size as f64);
        }
    }
    drop(f);
    if size > 0 && got != size {
        let _ = std::fs::remove_file(&part);
        bail!("The update download was incomplete. Try again.");
    }
    let want = asset["sha256"].as_str().unwrap_or("");
    let have: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if !want.is_empty() && have != want {
        let _ = std::fs::remove_file(&part);
        bail!("The update failed its checksum, so it wasn't installed.");
    }
    let _ = std::fs::remove_file(&target);
    std::fs::rename(&part, &target)?;
    Ok(target)
}

pub fn download(asset: &Value, progress: &dyn Fn(f64), cancel: Option<&Cancel>) -> Result<PathBuf> {
    let mut resp = ureq::get(asset["url"].as_str().unwrap_or("")).header("User-Agent", "StreamTranslator").call()?;
    let mut r = resp.body_mut().with_config().limit(u64::MAX).reader();
    save_verified(&mut r, asset, &dir(), progress, cancel)
}

/// Is this the installed app (Inno Setup's uninstaller sits next to the exe)?
pub fn is_installed() -> bool {
    std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("unins000.exe").is_file())).unwrap_or(false)
}

/// Run the installer once this process has exited, silently; the installer reopens the app when done.
/// The caller quits the app right after.
pub fn install(installer: &Path) -> Result<()> {
    if !is_installed() {
        bail!("Updates install into the installed app. You're running a development build.");
    }
    let q = |p: &Path| p.to_string_lossy().replace('\'', "''");
    let log = dir().join("install.log");
    let ps = format!(
        "Wait-Process -Id {} -Timeout 90 -ErrorAction SilentlyContinue; Start-Process -FilePath '{}' -ArgumentList '/SILENT','/SUPPRESSMSGBOXES','/NORESTART','/LOG=\"{}\"'",
        std::process::id(),
        q(installer),
        q(&log)
    );
    run_helper(&ps)?;
    Ok(())
}

/// Run a PowerShell command in the background that outlives this app. It needs CREATE_NO_WINDOW (a hidden
/// console of its own): with DETACHED_PROCESS PowerShell quietly does nothing (the 2.2.0/2.3.0 bug).
pub fn run_helper(ps: &str) -> std::io::Result<Child> {
    const NO_WINDOW: u32 = 0x0800_0000;
    const NEW_GROUP: u32 = 0x0000_0200;
    const BREAKAWAY: u32 = 0x0100_0000;
    let cmd = |flags: u32| Command::new("powershell").args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-Command", ps]).creation_flags(flags).spawn();
    // also leave any job object the app runs in, so closing the app can't take the helper with it
    cmd(NO_WINDOW | NEW_GROUP | BREAKAWAY).or_else(|_| cmd(NO_WINDOW | NEW_GROUP))
}
