//! Video files on this PC, treated like a VOD: the "stream" is the file itself, so the timeline,
//! frame previews, champion detection, cutting and burn-in all work the same way.

use std::{path::Path, sync::LazyLock};

use anyhow::Result;
use base64::Engine;
use regex::Regex;
use serde_json::{json, Value};
use st_core::{job_err, vod::VodInfo};

use crate::{
    ffmpeg::grab,
    tools::{command, ffmpeg_exe},
};

pub const SITE: &str = "local";
pub const CHANNEL: &str = "Video file";

/// What `ffmpeg -i` says about a file: length and whether it has picture and sound.
struct Probe {
    duration: f64,
    video: bool,
    audio: bool,
}

static DURATION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Duration: (\d+):(\d\d):(\d\d(?:\.\d+)?)").unwrap());
static STREAM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*Stream #\d+:\d+.*?: (Video|Audio): (\S+)").unwrap());

fn probe(path: &Path) -> Result<Probe> {
    let out = command(&ffmpeg_exe()?).args(["-hide_banner", "-nostdin", "-i"]).arg(path).output()?;
    let err = String::from_utf8_lossy(&out.stderr);
    let duration = DURATION
        .captures(&err)
        .map(|c| c[1].parse::<f64>().unwrap_or(0.0) * 3600.0 + c[2].parse::<f64>().unwrap_or(0.0) * 60.0 + c[3].parse::<f64>().unwrap_or(0.0))
        .unwrap_or(0.0);
    let mut p = Probe { duration, video: false, audio: false };
    for c in STREAM.captures_iter(&err) {
        // cover art in music files shows up as a one-frame "video" stream
        let still = matches!(&c[2], "mjpeg" | "png" | "bmp") && err.contains("(attached pic)");
        match &c[1] {
            "Video" if !still => p.video = true,
            "Audio" => p.audio = true,
            _ => {}
        }
    }
    Ok(p)
}

/// VodInfo for a local video file.
pub fn info(path: &Path) -> Result<VodInfo> {
    let p = probe(path)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !p.video && !p.audio {
        return job_err(format!("Couldn't read {name} as a video. Is it a video file?"));
    }
    if !p.video {
        return job_err(format!("{name} has no picture. Pick a video file (mp4, mkv, mov, webm…)."));
    }
    if !p.audio {
        return job_err(format!("{name} has no sound, so there's nothing to translate."));
    }
    if p.duration < 2.0 {
        return job_err(format!("Couldn't tell how long {name} is. The file may be damaged or still being written."));
    }
    let file = path.to_string_lossy().into_owned();
    let mut info = VodInfo {
        url: file.clone(),
        id: st_core::vod::local_id(path),
        title: name,
        channel: CHANNEL.into(),
        duration: p.duration,
        preview_stream: file.clone(),
        site: SITE.into(),
        ..Default::default()
    };
    for q in ["Source", "Audio"] {
        info.streams.insert(q.into(), file.clone());
    }
    if let Ok(jpg) = grab(&info, &file, p.duration * 0.1, "scale=480:-2", "mjpeg", 20) {
        info.thumbnail = data_url(&jpg);
    }
    Ok(info)
}

pub fn is_local(info: &VodInfo) -> bool {
    info.site == SITE
}

fn data_url(jpg: &[u8]) -> String {
    format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpg))
}

/// Timeline thumbnails in the same shape as Twitch's storyboards ({cols, rows, interval, w, h, sheets}),
/// as one sprite sheet. Frames are grabbed in parallel with fast seeks, so a two-hour file takes seconds.
pub fn storyboard(info: &VodInfo) -> Result<Value> {
    const W: u32 = 160;
    const H: u32 = 90;
    const COLS: u32 = 10;
    let n = ((info.duration / 5.0).ceil() as u32).clamp(10, 120);
    let rows = n.div_ceil(COLS);
    let interval = info.duration / n as f64;
    let vf = format!("scale={W}:{H}:force_original_aspect_ratio=decrease,pad={W}:{H}:-1:-1:black");
    let frames: Vec<Option<image::RgbImage>> = std::thread::scope(|sc| {
        let workers = 6;
        let hs: Vec<_> = (0..workers)
            .map(|k| {
                let vf = &vf;
                sc.spawn(move || {
                    (k..n)
                        .step_by(workers as usize)
                        .map(|i| {
                            let t = (i as f64 + 0.5) * interval;
                            let jpg = grab(info, &info.url, t, vf, "mjpeg", 20).ok()?;
                            Some((i, image::load_from_memory(&jpg).ok()?.to_rgb8()))
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let mut all: Vec<Option<image::RgbImage>> = vec![None; n as usize];
        for h in hs {
            for (i, img) in h.join().unwrap_or_default().into_iter().flatten() {
                all[i as usize] = Some(img);
            }
        }
        all
    });
    if frames.iter().all(Option::is_none) {
        return job_err("Couldn't read frames from the video.");
    }
    let mut sheet = image::RgbImage::new(W * COLS, H * rows);
    for (i, f) in frames.iter().enumerate() {
        if let Some(f) = f {
            let (x, y) = ((i as u32 % COLS) * W, (i as u32 / COLS) * H);
            image::imageops::overlay(&mut sheet, f, x as i64, y as i64);
        }
    }
    let mut jpg = vec![];
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 82).encode_image(&sheet)?;
    Ok(json!({"cols": COLS, "rows": rows, "interval": interval, "w": W, "h": H, "sheets": [data_url(&jpg)]}))
}
