//! Model downloads into the shared Hugging Face cache (%USERPROFILE%\.cache\huggingface\hub), laid out
//! like huggingface_hub without symlinks, so the Python app and this one share the same files.

use std::{
    io::{Read, Write},
    path::PathBuf,
};

use anyhow::{Context, Result};
use st_core::{cancel::Cancel, paths};

fn matches(name: &str, pattern: &str) -> bool {
    // simple globs: "*.bin", "vocabulary.*", "config.json"
    match pattern.split_once('*') {
        Some((a, b)) => name.starts_with(a) && name.ends_with(b) && name.len() >= a.len() + b.len(),
        None => name == pattern,
    }
}

/// The local snapshot of `repo`, downloading the files matching `patterns` if needed.
pub fn snapshot(repo: &str, patterns: &[&str], progress: &dyn Fn(f64), cancel: &Cancel) -> Result<PathBuf> {
    if let Some(p) = paths::hf_snapshot(repo) {
        return Ok(p);
    }
    let info: serde_json::Value = ureq::get(&format!("https://huggingface.co/api/models/{repo}/revision/main"))
        .header("User-Agent", "StreamTranslator")
        .call()
        .context("Couldn't reach Hugging Face to download the model")?
        .body_mut()
        .read_json()?;
    let sha = info["sha"].as_str().context("bad model info")?.to_string();
    let files: Vec<String> = info["siblings"]
        .as_array()
        .context("bad model info")?
        .iter()
        .filter_map(|s| s["rfilename"].as_str())
        .filter(|f| !f.contains('/') && patterns.iter().any(|p| matches(f, p)))
        .map(String::from)
        .collect();
    let root = paths::hf_hub().join(format!("models--{}", repo.replace('/', "--")));
    let snap = root.join("snapshots").join(&sha);
    std::fs::create_dir_all(&snap)?;
    let n = files.len().max(1) as f64;
    for (i, f) in files.iter().enumerate() {
        let dest = snap.join(f);
        if dest.is_file() {
            continue;
        }
        let mut resp = ureq::get(&format!("https://huggingface.co/{repo}/resolve/{sha}/{f}")).header("User-Agent", "StreamTranslator").call()?;
        let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse::<f64>().ok()).unwrap_or(0.0);
        let tmp = dest.with_extension("part");
        let mut out = std::fs::File::create(&tmp)?;
        let mut body = resp.body_mut().as_reader();
        let mut buf = vec![0u8; 1 << 20];
        let mut got = 0f64;
        loop {
            cancel.check()?;
            let k = body.read(&mut buf)?;
            if k == 0 {
                break;
            }
            out.write_all(&buf[..k])?;
            got += k as f64;
            if total > 0.0 {
                progress((i as f64 + got / total) / n);
            }
        }
        drop(out);
        std::fs::rename(&tmp, &dest)?;
    }
    std::fs::create_dir_all(root.join("refs"))?;
    std::fs::write(root.join("refs").join("main"), &sha)?;
    progress(1.0);
    Ok(snap)
}
