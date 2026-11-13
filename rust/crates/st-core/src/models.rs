//! Which models exist, where they live and whether they're on disk (no GPU code here, so the setup
//! screen can check them cheaply).

use std::path::{Path, PathBuf};

use crate::paths::{hf_snapshot, local_dir};

/// Whisper models offered in the app: name -> (Hugging Face repo, size in GB, label).
pub const WHISPER_MODELS: &[(&str, &str, f64, &str)] = &[
    ("large-v3", "Systran/faster-whisper-large-v3", 3.1, "Best"),
    ("large-v3-turbo", "mobiuslabsgmbh/faster-whisper-large-v3-turbo", 1.6, "Turbo"),
    ("medium", "Systran/faster-whisper-medium", 1.5, "Fast"),
    ("small", "Systran/faster-whisper-small", 0.5, "Fastest"),
];

/// Whisper model names -> Hugging Face repos (faster_whisper.utils._MODELS).
pub fn whisper_repo(name: &str) -> Option<&'static str> {
    Some(match name {
        "large-v3" | "large" => "Systran/faster-whisper-large-v3",
        "large-v3-turbo" | "turbo" => "mobiuslabsgmbh/faster-whisper-large-v3-turbo",
        "medium" => "Systran/faster-whisper-medium",
        "small" => "Systran/faster-whisper-small",
        "base" => "Systran/faster-whisper-base",
        "tiny" => "Systran/faster-whisper-tiny",
        _ => return None,
    })
}

/// Files a faster-whisper model needs.
pub const WHISPER_FILES: &[&str] = &["config.json", "preprocessor_config.json", "model.bin", "tokenizer.json", "vocabulary.*"];
/// Files an NLLB model needs.
pub const NLLB_FILES: &[&str] = &["*.bin", "*.json", "*.model", "*.txt"];

/// The model folder if it's on disk. `custom`: a faster-whisper model folder the user picked.
pub fn local_whisper(name: &str, custom: Option<&Path>) -> Option<PathBuf> {
    match custom {
        Some(p) if name == "custom" => p.join("model.bin").is_file().then(|| p.to_path_buf()),
        _ => whisper_repo(name).and_then(hf_snapshot),
    }
}

/// NLLB sizes: "standard" (600M) and "better" (1.3B) -> (repo, size in GB).
pub const NLLB: &[(&str, &str, f64)] = &[
    ("standard", "JustFrederik/nllb-200-distilled-600M-ct2-float16", 1.2),
    ("better", "JustFrederik/nllb-200-distilled-1.3B-ct2-float16", 2.7),
];

pub fn nllb_repo(size: &str) -> (&'static str, f64) {
    let e = NLLB.iter().find(|n| n.0 == size).unwrap_or(&NLLB[1]);
    (e.1, e.2)
}

pub fn nllb_downloaded(size: &str) -> bool {
    hf_snapshot(nllb_repo(size).0).is_some()
}

pub const UVR_URL: &str = "https://github.com/TRvlvr/model_repo/releases/download/all_public_uvr_models/UVR-MDX-NET-Voc_FT.onnx";
pub const UVR_SIZE: u64 = 66_762_490;

pub fn uvr_path() -> PathBuf {
    local_dir().join("models").join("UVR-MDX-NET-Voc_FT.onnx")
}

pub fn uvr_ready() -> bool {
    std::fs::metadata(uvr_path()).map(|m| m.len() == UVR_SIZE).unwrap_or(false)
}

pub const WESPEAKER_URL: &str = "https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM/resolve/main/voxceleb_resnet34_LM.onnx";
pub const WESPEAKER_SHA256: &str = "7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068";

pub fn wespeaker_path() -> PathBuf {
    local_dir().join("models").join("wespeaker-resnet34.onnx")
}

pub fn wespeaker_ready() -> bool {
    std::fs::metadata(wespeaker_path()).map(|m| m.len() > 20_000_000).unwrap_or(false)
}
