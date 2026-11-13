//! Keeping yt-dlp current without reinstalling the app (updater.py). Twitch changes its site often and
//! yt-dlp needs updating when it does. On start, the app compares the bundled/updated yt-dlp with the
//! latest official release; a newer folder build (yt-dlp_win.zip) is downloaded, checked against the
//! release's SHA2-256SUMS, and unpacked to %LOCALAPPDATA%\StreamTranslator\yt-dlp.

use std::io::Read;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use st_core::paths::local_dir;

/// The version of the yt-dlp the app uses now.
pub fn current_version() -> Option<String> {
    let exe = st_media::tools::ytdlp_exe().ok()?;
    let out = st_media::tools::command(&exe).arg("--version").output().ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!v.is_empty()).then_some(v)
}

fn vt(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse().unwrap_or(0)).collect()
}

fn fetch(url: &str) -> Result<Vec<u8>> {
    let mut resp = ureq::get(url).header("User-Agent", "StreamTranslator").call()?;
    let mut b = vec![];
    resp.body_mut().with_config().limit(200_000_000).reader().read_to_end(&mut b)?;
    Ok(b)
}

/// {current, latest, updated}
pub fn check_and_update() -> Result<Value> {
    let rel: Value = ureq::get("https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest")
        .header("User-Agent", "StreamTranslator")
        .header("Accept", "application/vnd.github+json")
        .call()?
        .body_mut()
        .read_json()?;
    let latest = rel["tag_name"].as_str().context("bad release info")?.to_string();
    let current = current_version();
    if vt(&latest) <= vt(current.as_deref().unwrap_or("0")) {
        return Ok(json!({"current": current, "latest": latest, "updated": false}));
    }
    let asset = |name: &str| -> Option<String> {
        rel["assets"].as_array()?.iter().find(|a| a["name"] == name).and_then(|a| a["browser_download_url"].as_str().map(String::from))
    };
    let (Some(zip_url), Some(sums_url)) = (asset("yt-dlp_win.zip"), asset("SHA2-256SUMS")) else {
        return Ok(json!({"current": current, "latest": latest, "updated": false, "error": "no Windows build published"}));
    };
    let sums = String::from_utf8_lossy(&fetch(&sums_url)?).to_string();
    let want = sums.lines().find_map(|l| l.strip_suffix(" yt-dlp_win.zip").or_else(|| l.strip_suffix("  yt-dlp_win.zip")).map(|h| h.trim().to_lowercase()));
    let blob = fetch(&zip_url)?;
    let have: String = Sha256::digest(&blob).iter().map(|b| format!("{b:02x}")).collect();
    if want.as_deref() != Some(have.as_str()) {
        bail!("yt-dlp download failed its checksum; not installed.");
    }
    let target = local_dir().join("yt-dlp");
    let tmp = local_dir().join(".yt-dlp-new");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(blob))?;
    z.extract(&tmp)?;
    if !tmp.join("yt-dlp.exe").is_file() {
        let _ = std::fs::remove_dir_all(&tmp);
        bail!("yt-dlp download looked incomplete; not installed.");
    }
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&tmp, &target)?;
    Ok(json!({"current": current, "latest": latest, "updated": true}))
}
