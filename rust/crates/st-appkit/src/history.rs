//! The library of past translations, history.json (history.py), with transcript search.

use std::{
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use regex::Regex;
use serde_json::{json, Map, Value};
use st_core::py;

static LOCK: Mutex<()> = Mutex::new(());

pub struct History {
    pub file: PathBuf,
}

impl History {
    pub fn new(dir: &Path) -> History {
        History { file: dir.join("history.json") }
    }

    fn load(&self) -> Vec<Value> {
        std::fs::read(&self.file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn save(&self, items: &[Value]) -> std::io::Result<()> {
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d)?;
        }
        let items: Vec<Value> = items.iter().take(500).cloned().collect();
        py::write_text(&self.file, &py::json_dumps(&Value::Array(items), Some(1), false))
    }

    /// Add an entry at the top (a clip replaces an older entry for the same folder).
    pub fn add(&self, entry: Map<String, Value>) -> std::io::Result<Value> {
        let mut e = Map::new();
        e.insert("id".into(), crate::new_id(12).into());
        e.insert("created".into(), py::f(crate::now()));
        for (k, v) in entry {
            e.insert(k, v);
        }
        let e = Value::Object(e);
        let _g = LOCK.lock().unwrap();
        let live = e["type"] == "live";
        let mut items: Vec<Value> = self.load().into_iter().filter(|x| live || x.get("folder") != e.get("folder")).collect();
        items.insert(0, e.clone());
        self.save(&items)?;
        Ok(e)
    }

    pub fn update(&self, id: &str, changes: &Value) -> std::io::Result<Option<Value>> {
        let _g = LOCK.lock().unwrap();
        let mut items = self.load();
        if let Some(e) = items.iter_mut().find(|e| e["id"] == id) {
            if let (Some(m), Some(c)) = (e.as_object_mut(), changes.as_object()) {
                for (k, v) in c {
                    m.insert(k.clone(), v.clone());
                }
            }
            let out = e.clone();
            self.save(&items)?;
            return Ok(Some(out));
        }
        Ok(None)
    }

    pub fn remove(&self, id: &str) -> std::io::Result<()> {
        let _g = LOCK.lock().unwrap();
        let items: Vec<Value> = self.load().into_iter().filter(|e| e["id"] != id).collect();
        self.save(&items)
    }

    /// All entries, each with "exists" (its files are still on disk).
    pub fn listing(&self) -> Vec<Value> {
        let mut items = self.load();
        for e in items.iter_mut() {
            let path = |k: &str| e.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).is_some_and(|p| Path::new(p).exists());
            let exists = if e["type"] == "clip" { path("folder") } else { path("transcript") };
            e["exists"] = exists.into();
        }
        items
    }

    /// Search titles, channels and transcript text: {id: {hits, snippets: [{time, text}]}}.
    pub fn search(&self, q: &str, limit_hits: usize) -> Map<String, Value> {
        static TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[(?:VOD\s+)?([\d:]+)").unwrap());
        static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[[^\]]*\]\s*").unwrap());
        let q = q.trim().to_lowercase();
        let mut out = Map::new();
        if q.is_empty() {
            return out;
        }
        for e in self.load() {
            let mut hits = 0;
            let mut snippets = vec![];
            let s = |k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            if format!("{} {}", s("title"), s("channel")).to_lowercase().contains(&q) {
                hits += 1;
            }
            let tx = s("transcript");
            if !tx.is_empty() {
                if let Ok(bytes) = std::fs::read(&tx) {
                    for line in String::from_utf8_lossy(&bytes).lines() {
                        if line.to_lowercase().contains(&q) {
                            hits += 1;
                            if snippets.len() < limit_hits {
                                let l = line.trim();
                                let time = TIME.captures(l).map(|c| c[1].to_string()).unwrap_or_default();
                                let text: String = TAG.replace(l, "").chars().take(200).collect();
                                snippets.push(json!({"time": time, "text": text}));
                            }
                        }
                    }
                }
            }
            if hits > 0 {
                out.insert(s("id"), json!({"hits": hits, "snippets": snippets}));
            }
        }
        out
    }
}
