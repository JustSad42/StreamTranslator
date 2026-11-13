//! Finding ffmpeg and yt-dlp, and running them without console windows.

use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};

use st_core::JobError;

/// Child processes never flash a console window (Python: CREATE_NO_WINDOW).
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn command(exe: &Path) -> Command {
    let mut c = Command::new(exe);
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(name)).find(|p| p.is_file())
}

/// The installer's own copy next to the app, else one on PATH.
pub fn ffmpeg_exe() -> Result<PathBuf, JobError> {
    if let Some(d) = exe_dir() {
        for p in [d.join("ffmpeg").join("ffmpeg.exe"), d.join("ffmpeg.exe")] {
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    on_path("ffmpeg.exe").ok_or_else(|| JobError("ffmpeg was not found. Install it with:  winget install Gyan.FFmpeg".into()))
}

/// yt-dlp: the official folder build ("yt-dlp_win.zip", starts in 0.3 s; the single-file exe unpacks
/// itself on every run and takes 1.5 s). Order: the self-updated copy in %LOCALAPPDATA%\StreamTranslator\yt-dlp,
/// the bundled one next to the app, ST_YTDLP (development), PATH.
pub fn ytdlp_exe() -> Result<PathBuf, JobError> {
    let mut candidates = vec![st_core::paths::local_dir().join("yt-dlp").join("yt-dlp.exe")];
    if let Some(d) = exe_dir() {
        candidates.push(d.join("yt-dlp").join("yt-dlp.exe"));
        candidates.push(d.join("yt-dlp.exe"));
    }
    if let Some(p) = std::env::var_os("ST_YTDLP") {
        candidates.push(PathBuf::from(p));
    }
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .or_else(|| on_path("yt-dlp.exe"))
        .ok_or_else(|| JobError("yt-dlp.exe was not found next to the app.".into()))
}
