//! settings.json (app.py DEFAULTS, load_settings, m_settings_save). Keys this version doesn't know
//! are kept, so going back to the Python app loses nothing.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use st_core::py;

pub fn defaults() -> Map<String, Value> {
    let out_dir = PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Videos").join("StreamTranslator");
    let v = json!({
        "my_channel": "", "out_dir": out_dir.to_string_lossy(), "model": "large-v3",
        "live_model": "large-v3", "preload": true, "quality": "720p", "position": "Above game HUD",
        "size": "Medium", "burn": true, "orig": false, "language": "", "use_glossary": true, "isolate": false,
        "live_language": "", "live_orig": true, "live_isolate": false, "live_chat": true,
        "overlay_font": 30, "overlay_bg": 0.55, "ui_scale": "auto", "notify": true, "auto_update": true,
        "method": "two-step", "translator": "better", "auto_details": true, "live_method": "two-step",
        "custom_model": "", "setup_done": false, "app_update": true, "speakers": false,
        "last_version": "", "updated_from": "", "skip_version": "",
    });
    v.as_object().unwrap().clone()
}

pub struct Settings {
    pub values: Map<String, Value>,
    path: PathBuf,
}

impl Settings {
    /// Defaults, overlaid with the saved file.
    pub fn load(path: &Path) -> Settings {
        let mut values = defaults();
        if let Some(Value::Object(saved)) = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
            for (k, v) in saved {
                values.insert(k, v);
            }
        }
        Settings { values, path: path.to_path_buf() }
    }

    /// Change known keys only (like Python), then save.
    pub fn save(&mut self, data: &Value) -> std::io::Result<&Map<String, Value>> {
        let known = defaults();
        if let Some(m) = data.as_object() {
            for (k, v) in m {
                if known.contains_key(k) {
                    self.values.insert(k.clone(), v.clone());
                }
            }
        }
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        py::write_text(&self.path, &py::json_dumps(&Value::Object(self.values.clone()), Some(2), true))?;
        Ok(&self.values)
    }

    pub fn str(&self, k: &str) -> String {
        self.values.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
    }

    pub fn bool(&self, k: &str) -> bool {
        match self.values.get(k) {
            Some(Value::Bool(b)) => *b,
            Some(Value::Number(n)) => n.as_f64() != Some(0.0),
            Some(Value::String(s)) => !s.is_empty(),
            _ => false,
        }
    }

    pub fn get(&self, k: &str) -> Value {
        self.values.get(k).cloned().unwrap_or(Value::Null)
    }
}
