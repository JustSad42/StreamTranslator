//! Talking to the outside world: yt-dlp (VOD info), ffmpeg (download, decode, frames, burn-in),
//! and Kick's video API.

pub mod ffmpeg;
pub mod hub;
pub mod local;
pub mod tools;
pub mod ytdlp;

/// Progress callback: 0..1, or a negative value for "busy".
pub type Progress<'a> = &'a (dyn Fn(f64) + Send + Sync);
