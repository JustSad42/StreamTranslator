//! Making NVIDIA's libraries (cuBLAS, cuDNN) and ONNX Runtime findable (cuda.py).
//!
//! The DLLs come from the copy StreamTranslator downloads (%LOCALAPPDATA%\StreamTranslator\cuda\*\bin)
//! or an existing install the setup screen found (gpu_libs.json). CTranslate2 loads cuBLAS by name at
//! runtime; cuDNN is delay-loaded by the exe, so both only need to be on the search path before the
//! first GPU model loads.

use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use st_core::paths::local_dir;

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

/// Folders holding cuBLAS / cuDNN DLLs.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![];
    if let Ok(rd) = std::fs::read_dir(local_dir().join("cuda")) {
        let mut found: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).filter(|p| p.is_dir()).collect();
        found.sort();
        dirs.extend(found);
    }
    if let Some(saved) = std::fs::read(local_dir().join("gpu_libs.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v["dir"].as_str().map(PathBuf::from))
    {
        if saved.is_dir() {
            dirs.push(saved);
        }
    }
    dirs
}

fn find_dll(name: &str, dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|d| d.join(name).is_file())
        || std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

static SETUP: Mutex<bool> = Mutex::new(false);

/// Put the GPU library folders on the DLL search path. Safe to call again after the setup screen
/// downloads them (`force`).
pub fn setup(force: bool) {
    let mut done = SETUP.lock().unwrap();
    if *done && !force {
        return;
    }
    *done = true;
    let dirs = search_dirs();
    for d in &dirs {
        let w: Vec<u16> = d.as_os_str().encode_wide_null();
        unsafe {
            let _ = windows::Win32::System::LibraryLoader::AddDllDirectory(windows::core::PCWSTR(w.as_ptr()));
        }
    }
    if !dirs.is_empty() {
        let mut all = dirs.clone();
        if let Some(p) = std::env::var_os("PATH") {
            all.extend(std::env::split_paths(&p));
        }
        if let Ok(p) = std::env::join_paths(all) {
            // SAFETY: called during setup, before worker threads read the environment.
            unsafe { std::env::set_var("PATH", p) };
        }
    }
}

trait WideNull {
    fn encode_wide_null(&self) -> Vec<u16>;
}
impl WideNull for std::ffi::OsStr {
    fn encode_wide_null(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain([0]).collect()
    }
}

/// Is an NVIDIA GPU usable for Whisper/NLLB? (driver present, cuBLAS 12 and cuDNN 9 findable.)
/// Without this check a missing cuDNN would crash the delay-load instead of falling back to the CPU.
pub fn cuda_ready() -> bool {
    setup(false);
    let dirs = search_dirs();
    let driver = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or("C:\\Windows".into())).join("System32").join("nvcuda.dll").is_file();
    driver && find_dll("cublas64_12.dll", &dirs) && find_dll("cublasLt64_12.dll", &dirs) && find_dll("cudnn64_9.dll", &dirs) && ct2rs_sys::get_device_count(ct2rs_sys::Device::CUDA) > 0
}

/// A file shipped with the app (onnxruntime.dll, silero_vad_v6.onnx...): next to the exe, in its
/// "assets" folder, in ST_ASSETS (development), or in %LOCALAPPDATA%\StreamTranslator\models.
pub fn asset(name: &str) -> Option<PathBuf> {
    let mut c = vec![];
    if let Some(d) = exe_dir() {
        c.push(d.join(name));
        c.push(d.join("assets").join(name));
    }
    if let Some(d) = std::env::var_os("ST_ASSETS") {
        c.push(PathBuf::from(d).join(name));
    }
    c.push(local_dir().join("models").join(name));
    c.into_iter().find(|p| p.is_file())
}

static ORT: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();

/// Load onnxruntime.dll (Microsoft's DirectML build, shipped next to the exe). ONNX Runtime is loaded
/// at runtime because its prebuilt static library uses a different C runtime than CTranslate2.
pub fn ort_env() -> anyhow::Result<&'static ort::environment::Environment> {
    ORT.get_or_init(|| {
        let dll = asset("onnxruntime.dll").ok_or("onnxruntime.dll is missing next to the app")?;
        ort::init_from(&dll).map_err(|e| e.to_string())?.with_name("StreamTranslator").build().map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| anyhow::anyhow!(e.clone()))
}
