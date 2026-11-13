//! The app's own plumbing, shared with the Python app's files: settings, history, logs and crash
//! detection, app updates from GitHub, the setup screen's components, yt-dlp updates, champion list.
//! No GPU and no window code, so all of it is testable with plain `cargo test`.

pub mod appupdate;
pub mod champions;
pub mod components;
pub mod history;
pub mod logs;
pub mod settings;
pub mod ytdlp_update;

/// This build's version (3.0.0 for the first Rust release).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Short random id (hex).
pub fn new_id(len: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    while out.len() < len {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(len);
    out
}

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}
