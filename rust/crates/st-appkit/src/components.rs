//! What the first-run setup screen checks and downloads (components.py): NVIDIA's GPU libraries
//! (downloaded from PyPI, checksum-verified, or reused from another install), models, ffmpeg.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use st_core::{models, paths::local_dir};

/// exact versions this app is tested with
pub const GPU_PACKAGES: &[(&str, &str)] = &[("nvidia-cublas-cu12", "12.9.2.10"), ("nvidia-cudnn-cu12", "9.28.1.4")];
pub const NEEDED: &[&str] = &["cublas64_12.dll", "cublasLt64_12.dll", "cudnn64_9.dll", "cudnn_ops64_9.dll", "cudnn_cnn64_9.dll"];

fn cuda_dir() -> PathBuf {
    local_dir().join("cuda")
}

fn has_all(folder: &Path) -> bool {
    NEEDED.iter().all(|n| folder.join(n).exists())
}

fn has_all_split(root: &Path) -> bool {
    let mut names = std::collections::HashSet::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for sub in rd.flatten() {
            if let Ok(files) = std::fs::read_dir(sub.path().join("bin")) {
                names.extend(files.flatten().map(|f| f.file_name().to_string_lossy().into_owned()));
            }
        }
    }
    NEEDED.iter().all(|n| names.contains(*n))
}

fn glob_dirs(root: &Path, prefix: &str, limit: usize) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(prefix))).collect())
        .unwrap_or_default();
    out.truncate(limit);
    out
}

/// Folders that might already hold cuBLAS 12 + cuDNN 9.
fn candidate_dirs() -> Vec<(PathBuf, String)> {
    let mut out = vec![];
    if let Some(c) = std::env::var_os("CUDA_PATH") {
        out.push((PathBuf::from(c).join("bin"), "CUDA Toolkit".to_string()));
    }
    if let Some(p) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&p) {
            if d.to_string_lossy().to_lowercase().contains("cudnn") {
                out.push((d, "cuDNN on PATH".into()));
            }
        }
    }
    let home = PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default());
    let pf = PathBuf::from(std::env::var_os("ProgramFiles").unwrap_or("C:\\Program Files".into()));
    for root in [home.join("Desktop"), home.join("Downloads"), PathBuf::from("C:\\"), pf] {
        let mut ds = glob_dirs(&root, "Faster-Whisper-XXL", 3);
        if let Ok(rd) = std::fs::read_dir(&root) {
            for sub in rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).take(200) {
                ds.extend(glob_dirs(&sub, "Faster-Whisper-XXL", 3));
                if ds.len() > 6 {
                    break;
                }
            }
        }
        for d in ds {
            out.push((d.clone(), "Faster-Whisper-XXL".into()));
            out.push((d.join("_xxl_data"), "Faster-Whisper-XXL".into()));
        }
    }
    let se = PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("Subtitle Edit").join("Whisper");
    if let Ok(rd) = std::fs::read_dir(se) {
        out.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).map(|d| (d, "Subtitle Edit".to_string())));
    }
    out
}

fn saved_dir() -> Option<(String, String)> {
    let v: Value = serde_json::from_slice(&std::fs::read(local_dir().join("gpu_libs.json")).ok()?).ok()?;
    let d = v["dir"].as_str()?.to_string();
    (Path::new(&d).is_dir() && has_all(Path::new(&d))).then(|| (d, v["source"].as_str().unwrap_or("").to_string()))
}

/// Where the GPU libraries come from, if anywhere.
pub fn gpu_libs() -> Value {
    let cd = cuda_dir();
    if GPU_PACKAGES.iter().all(|(p, _)| cd.join(p.split('-').nth(1).unwrap()).join("bin").is_dir()) && has_all_split(&cd) {
        return json!({"ok": true, "source": "downloaded by StreamTranslator", "dir": cd.to_string_lossy()});
    }
    if let Some((d, s)) = saved_dir() {
        return json!({"ok": true, "source": s, "dir": d});
    }
    for (d, label) in candidate_dirs() {
        if d.is_dir() && has_all(&d) {
            return json!({"ok": true, "source": format!("reusing {label}"), "dir": d.to_string_lossy(), "found": true});
        }
    }
    json!({"ok": false, "source": null, "dir": null, "size_gb": 1.3})
}

pub fn use_existing(folder: &str, label: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(local_dir())?;
    std::fs::write(local_dir().join("gpu_libs.json"), st_core::py::json_dumps(&json!({"dir": folder, "source": label}), None, true))
}

/// Download NVIDIA's official cuBLAS/cuDNN wheels from PyPI and keep only the DLLs.
pub fn download_gpu_libs(progress: &dyn Fn(f64, &str), cancel: &st_core::cancel::Cancel) -> Result<()> {
    let mut plan = vec![];
    for (pkg, ver) in GPU_PACKAGES {
        let meta: Value = ureq::get(&format!("https://pypi.org/pypi/{pkg}/{ver}/json")).header("User-Agent", "StreamTranslator").call()?.body_mut().read_json()?;
        let wheel = meta["urls"].as_array().context("bad PyPI answer")?.iter().find(|u| u["filename"].as_str().unwrap_or("").ends_with("win_amd64.whl")).cloned().context("no Windows package on PyPI")?;
        plan.push((*pkg, wheel));
    }
    let total: f64 = plan.iter().map(|(_, w)| w["size"].as_f64().unwrap_or(1.0)).sum();
    let mut done = 0f64;
    std::fs::create_dir_all(cuda_dir())?;
    for (pkg, wheel) in plan {
        let tmp = cuda_dir().join(format!("{}.part", wheel["filename"].as_str().unwrap()));
        let mut h = Sha256::new();
        {
            let mut resp = ureq::get(wheel["url"].as_str().unwrap()).header("User-Agent", "StreamTranslator").call()?;
            let mut r = resp.body_mut().with_config().limit(u64::MAX).reader();
            let mut f = std::fs::File::create(&tmp)?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                if cancel.is_set() {
                    drop(f);
                    let _ = std::fs::remove_file(&tmp);
                    bail!("Cancelled");
                }
                let k = r.read(&mut buf)?;
                if k == 0 {
                    break;
                }
                f.write_all(&buf[..k])?;
                h.update(&buf[..k]);
                done += k as f64;
                progress(done / total * 0.95, &format!("Downloading {pkg} ({:.2} of {:.2} GB)", done / 1e9, total / 1e9));
            }
        }
        let have: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if Some(have.as_str()) != wheel["digests"]["sha256"].as_str() {
            let _ = std::fs::remove_file(&tmp);
            bail!("{pkg} failed its checksum. Nothing was installed; try again.");
        }
        progress(done / total * 0.95, &format!("Unpacking {pkg}…"));
        let dest = cuda_dir().join(pkg.split('-').nth(1).unwrap()).join("bin");
        std::fs::create_dir_all(&dest)?;
        let mut z = zip::ZipArchive::new(std::fs::File::open(&tmp)?)?;
        for i in 0..z.len() {
            let mut entry = z.by_index(i)?;
            let name = entry.name().to_string();
            if name.to_lowercase().ends_with(".dll") && name.contains("/bin/") && !name.contains("..") {
                let file = Path::new(&name).file_name().unwrap().to_owned();
                let mut out = std::fs::File::create(dest.join(file))?;
                std::io::copy(&mut entry, &mut out)?;
            }
        }
        let _ = std::fs::remove_file(&tmp);
    }
    progress(1.0, "GPU libraries ready");
    Ok(())
}

pub fn model_status() -> Value {
    let whisper: Map<String, Value> = models::WHISPER_MODELS
        .iter()
        .map(|(name, _, gb, label)| (name.to_string(), json!({"label": label, "gb": gb, "ok": models::local_whisper(name, None).is_some()})))
        .collect();
    let translator: Map<String, Value> = models::NLLB.iter().map(|(k, _, gb)| (k.to_string(), json!({"gb": gb, "ok": models::nllb_downloaded(k)}))).collect();
    json!({"whisper": whisper, "translator": translator, "isolation": {"gb": 0.07, "ok": models::uvr_ready()}})
}

/// A faster-whisper (CTranslate2) model folder: model.bin + config.json + tokenizer.
pub fn check_model_folder(folder: &str) -> Result<String> {
    let p = Path::new(folder);
    let any = |f: &dyn Fn(&str) -> bool| std::fs::read_dir(p).map(|rd| rd.flatten().any(|e| f(&e.file_name().to_string_lossy()))).unwrap_or(false);
    if !p.join("model.bin").exists() || !p.join("config.json").exists() {
        if p.join("pytorch_model.bin").exists() || any(&|n| n.ends_with(".pt")) {
            bail!("That's an OpenAI/PyTorch Whisper model. This app needs a faster-whisper (CTranslate2) model: a folder with model.bin and config.json.");
        }
        if any(&|n| n.starts_with("ggml") && n.ends_with(".bin")) {
            bail!("That's a whisper.cpp model, which this app can't use. It needs a faster-whisper (CTranslate2) model folder with model.bin and config.json.");
        }
        bail!("That folder doesn't look like a faster-whisper model (it needs model.bin and config.json).");
    }
    if !(p.join("tokenizer.json").exists() || p.join("vocabulary.json").exists() || p.join("vocabulary.txt").exists()) {
        bail!("The model folder is missing its tokenizer/vocabulary file.");
    }
    Ok(folder.to_string())
}

pub fn ffmpeg() -> Value {
    match st_media::tools::ffmpeg_exe() {
        Ok(p) => {
            let bundled = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| p.starts_with(d))).unwrap_or(false);
            json!({"ok": true, "source": if bundled { "bundled with the app" } else { "installed on this PC" }, "path": p.to_string_lossy()})
        }
        Err(_) => json!({"ok": false}),
    }
}

pub fn has_nvidia() -> bool {
    st_media::tools::command(Path::new("nvidia-smi"))
        .arg("-L")
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).contains("GPU"))
        .unwrap_or(false)
}

/// First line of `nvidia-smi --query-gpu=name`, e.g. "NVIDIA GeForce RTX 4080".
pub fn gpu_name() -> String {
    st_media::tools::command(Path::new("nvidia-smi"))
        .args(["--query-gpu=name", "--format=csv,noheader"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8_lossy(&o.stdout).lines().next().map(|l| l.trim().to_string()))
        .unwrap_or_default()
}

pub fn status() -> Value {
    json!({"gpu": gpu_libs(), "models": model_status(), "ffmpeg": ffmpeg(), "has_nvidia": has_nvidia()})
}
