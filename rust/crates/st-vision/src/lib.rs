//! Reading the game off the video (detect.py): champions by their portraits, players by their
//! nameplates (Windows OCR). Both feed Whisper's prompt and the clean-up for that one job.

pub mod matcher;
pub mod ocr;
pub mod snap;

use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use anyhow::Result;
use rayon::prelude::*;
use serde_json::{json, Value};
use st_core::{cancel::Cancel, glossary::Glossary, glossary::DDRAGON, job_err, ocrtext::players_in, paths::local_dir, vod::VodInfo};

use matcher::Img;

pub fn icon_dir(version: &str) -> PathBuf {
    local_dir().join("icons").join(version)
}

type Icons = Vec<(String, Vec<Img>)>;
static ICONS: OnceLock<Mutex<Option<(String, std::sync::Arc<Icons>)>>> = OnceLock::new();

/// Riot's champion icons at every search size (downloaded once per Data Dragon version).
fn icons(gl: &Glossary) -> Result<std::sync::Arc<Icons>> {
    let Some(version) = gl.version.clone() else {
        return job_err("Champion icons need the champion list from Riot (connect once and restart).");
    };
    let cell = ICONS.get_or_init(|| Mutex::new(None));
    if let Some((v, i)) = cell.lock().unwrap().as_ref() {
        if *v == version {
            return Ok(i.clone());
        }
    }
    let dir = icon_dir(&version);
    std::fs::create_dir_all(&dir)?;
    let champs: Vec<(String, String)> = gl.champions.iter().filter_map(|c| Some((c.name.clone(), c.id.clone()?))).collect();
    champs.par_iter().for_each(|(_, id)| {
        let p = dir.join(format!("{id}.png"));
        if !p.exists() {
            if let Ok(mut r) = ureq::get(&format!("{DDRAGON}/cdn/{version}/img/champion/{id}.png")).header("User-Agent", "StreamTranslator").call() {
                if let Ok(b) = r.body_mut().read_to_vec() {
                    let _ = std::fs::write(&p, b);
                }
            }
        }
    });
    let loaded = std::sync::Arc::new(matcher::load_icons(&dir, &champs));
    *cell.lock().unwrap() = Some((version, loaded.clone()));
    Ok(loaded)
}

/// Champions and player names seen in `start..end` of the VOD, as saved in details.json:
/// {"champions": [...], "champion_scores": [[name, score], ...], "players": [...]}
pub fn game_details(info: &VodInfo, start: f64, end: f64, gl: &Glossary, cancel: &Cancel, samples: usize) -> Result<Value> {
    let span = (end - start).max(1.0);
    let times: Vec<f64> = (0..samples).map(|i| start + span * (i + 1) as f64 / (samples + 1) as f64).collect();
    let pngs: Vec<Vec<u8>> = std::thread::scope(|s| {
        let hs: Vec<_> = times.iter().map(|&t| s.spawn(move || st_media::ffmpeg::frame_png(info, t, 1920).ok())).collect();
        hs.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    });
    if cancel.is_set() {
        return Ok(json!({}));
    }
    let frames: Vec<Img> = pngs
        .par_iter()
        .filter_map(|p| {
            let i = Img::from_png(p).ok()?;
            Some(i.resize_area(1280, i.h * 1280 / i.w))
        })
        .collect();
    let champs = if frames.is_empty() { vec![] } else { matcher::champions_in(&frames, &icons(gl)?) };
    let lines: Vec<Vec<String>> = pngs.iter().map(|p| ocr::ocr(p).unwrap_or_default()).collect();
    let players = players_in(&lines, &gl.champion_keys());
    Ok(json!({
        "champions": champs.iter().map(|c| c.0.clone()).collect::<Vec<_>>(),
        "champion_scores": champs.iter().map(|c| json!([c.0, c.1 as f64])).collect::<Vec<_>>(),
        "players": players,
    }))
}
