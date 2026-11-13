//! Where things live on disk (shared with the Python app).

use std::path::PathBuf;

fn env_dir(var: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()))
}

/// %APPDATA%\StreamTranslator (settings, glossary, history, champions)
pub fn data_dir() -> PathBuf {
    env_dir("APPDATA").join("StreamTranslator")
}

/// %LOCALAPPDATA%\StreamTranslator (GPU libraries, models, icons, logs, updates)
pub fn local_dir() -> PathBuf {
    env_dir("LOCALAPPDATA").join("StreamTranslator")
}

/// The Hugging Face cache the Python app downloads Whisper and NLLB into.
pub fn hf_hub() -> PathBuf {
    if let Some(p) = std::env::var_os("HF_HUB_CACHE") {
        return PathBuf::from(p);
    }
    if let Some(p) = std::env::var_os("HF_HOME") {
        return PathBuf::from(p).join("hub");
    }
    env_dir("USERPROFILE").join(".cache").join("huggingface").join("hub")
}

/// A downloaded Hugging Face repo snapshot ("org/name"), if present.
pub fn hf_snapshot(repo: &str) -> Option<PathBuf> {
    let dir = hf_hub().join(format!("models--{}", repo.replace('/', "--")));
    let rev = std::fs::read_to_string(dir.join("refs").join("main")).ok()?;
    let snap = dir.join("snapshots").join(rev.trim());
    snap.is_dir().then_some(snap)
}
