//! Loaded models, kept between jobs. Two Whisper models can stay loaded (live mode and a clip job may
//! use different ones); one translator at a time (switching size frees the other's GPU memory first).

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use st_core::cancel::Cancel;
use st_ml::{nllb::Translator, whisper::WhisperModel};

#[derive(Default)]
pub struct Engines {
    whisper: Mutex<Vec<(String, Arc<WhisperModel>)>>,
    translator: Mutex<Option<Arc<Translator>>>,
}

impl Engines {
    pub fn new() -> Self {
        st_ml::gpu::setup(false);
        Self::default()
    }

    /// Load (or reuse) a Whisper model.
    pub fn whisper(&self, name: &str, custom: Option<&Path>, log: &dyn Fn(&str), progress: &dyn Fn(f64), cancel: &Cancel) -> Result<Arc<WhisperModel>> {
        let mut cache = self.whisper.lock().unwrap();
        if let Some((_, m)) = cache.iter().find(|(n, _)| n == name) {
            return Ok(m.clone());
        }
        let m = Arc::new(WhisperModel::load(name, custom, log, progress, cancel)?);
        if m.device == "GPU" {
            log(&format!("Loaded Whisper {name} on the GPU."));
        }
        while cache.len() >= 2 {
            cache.remove(0);
        }
        cache.push((name.to_string(), m.clone()));
        Ok(m)
    }

    /// "GPU" / "CPU" if that Whisper model is loaded.
    pub fn whisper_device(&self, name: &str) -> Option<&'static str> {
        self.whisper.lock().unwrap().iter().find(|(n, _)| n == name).map(|(_, m)| m.device)
    }

    pub fn translator(&self, size: &str, status: &dyn Fn(&str), progress: &dyn Fn(f64), cancel: &Cancel) -> Result<Arc<Translator>> {
        let size = if size == "standard" { "standard" } else { "better" };
        let mut t = self.translator.lock().unwrap();
        if let Some(tr) = t.as_ref().filter(|tr| tr.size == size) {
            return Ok(tr.clone());
        }
        *t = None; // free the other size's GPU memory first
        let tr = Arc::new(Translator::load(size, status, progress, cancel)?);
        *t = Some(tr.clone());
        Ok(tr)
    }
}
