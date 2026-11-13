//! The backend the interface talks to (app.py's Backend). Every `api(method, params)` call from app.js
//! arrives here; each gets a "reply" event back. Long work reports through events. Method names, params
//! and result shapes are the Python app's, so app.js runs unchanged.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
};

use anyhow::{anyhow, bail, Result};
use base64::Engine as _;
use serde_json::{json, Map, Value};
use st_appkit::{appupdate, champions, components, history::History, logs, settings::Settings, VERSION};
use st_core::{
    cancel::Cancel,
    glossary::Glossary,
    langs::{lang_name, nllb_code},
    paths, py,
    subs::{read_srt, write_srt_cues, Cue},
    sync::{classify, Kind},
    vod::VodInfo,
    Cancelled, JobError,
};
use st_job::{read_meta, reburn, run_job, write_meta, Callbacks, Engines, JobParams};
use st_media::{ffmpeg, ytdlp};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;

#[path = "live.rs"]
mod live;
pub use live::OVERLAY_PAGE;

/// WebView2 flags. Every window must use the same ones (WebView2 refuses a second set in one app).
/// ST_DEVTOOLS_PORT (development only) exposes the pages to automated UI tests.
pub fn browser_args() -> String {
    let mut a = "--autoplay-policy=no-user-gesture-required".to_string();
    if let Ok(port) = std::env::var("ST_DEVTOOLS_PORT") {
        a += &format!(" --remote-debugging-port={port}");
    }
    a
}

/// clamp(screen height / 1200, 1.0, 2.5), rounded to 5%: 1080p -> 100%, 1440p -> 120%, 4K at 100% -> 180%.
pub fn auto_scale(logical_height: f64) -> f64 {
    ((logical_height / 1200.0).clamp(1.0, 2.5) * 20.0).round_ties_even() / 20.0
}

/// An http://asset.localhost URL the page can load a local file from (Tauri's convertFileSrc).
pub fn file_url(path: &Path) -> Option<String> {
    if !path.exists() {
        return None;
    }
    let s = path.to_string_lossy();
    let mut enc = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => enc.push(b as char),
            _ => enc.push_str(&format!("%{b:02X}")),
        }
    }
    Some(format!("http://asset.localhost/{enc}"))
}

/// Open a file, folder or link with its default app.
pub fn shell_open(target: &str) {
    use windows::{core::PCWSTR, Win32::UI::Shell::ShellExecuteW, Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL};
    let w: Vec<u16> = target.encode_utf16().chain([0]).collect();
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    unsafe {
        ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(w.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

struct Job {
    id: String,
    kind: String,
    params: Value,
    label: String,
    status: String,
    error: Option<String>,
    result: Option<Value>,
}

impl Job {
    fn view(&self) -> Value {
        json!({"id": self.id, "kind": self.kind, "label": self.label, "status": self.status, "error": self.error,
               "result": self.result, "vod_id": self.params.get("vod_id"), "start": self.params.get("start")})
    }
}

pub struct Backend {
    app: AppHandle,
    settings: Mutex<Settings>,
    glossary: Arc<Mutex<Glossary>>,
    history: History,
    engines: Arc<Engines>,
    vods: Mutex<HashMap<String, VodInfo>>,
    jobs: Mutex<Vec<Job>>,
    qcond: Condvar,
    cancel_current: Mutex<Option<Cancel>>,
    snap_cancel: Mutex<Option<Cancel>>,
    gpu: Mutex<Option<String>>,
    update_info: Mutex<Value>,
    app_update: Mutex<Value>,
    updating: AtomicBool,
    frames: AtomicUsize,
    zoom: Mutex<f64>,
    live: Mutex<Option<Arc<st_live::LiveSession>>>,
    chat: Mutex<Option<Arc<st_live::chat::ChatSession>>>,
    obs: Mutex<Option<st_live::obs::ObsServer>>,
    overlay: Mutex<live::OverlayStyle>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}
fn f(v: &Value, k: &str) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0)
}
/// Python truthiness with a default for missing keys.
fn b(v: &Value, k: &str, default: bool) -> bool {
    match v.get(k) {
        None => default,
        Some(Value::Bool(x)) => *x,
        Some(Value::Null) => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(x)) => !x.is_empty(),
        Some(_) => true,
    }
}
fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl Backend {
    pub fn new(app: AppHandle) -> Backend {
        let settings = Settings::load(&paths::data_dir().join("settings.json"));
        let be = Backend {
            app,
            settings: Mutex::new(settings),
            glossary: Arc::new(Mutex::new(Glossary::load(&paths::data_dir()))),
            history: History::new(&paths::data_dir()),
            engines: Arc::new(Engines::new()),
            vods: Mutex::new(HashMap::new()),
            jobs: Mutex::new(vec![]),
            qcond: Condvar::new(),
            cancel_current: Mutex::new(None),
            snap_cancel: Mutex::new(None),
            gpu: Mutex::new(None),
            update_info: Mutex::new(json!({"current": null})),
            app_update: Mutex::new(json!({"current": VERSION})),
            updating: AtomicBool::new(false),
            frames: AtomicUsize::new(0),
            zoom: Mutex::new(1.0),
            live: Mutex::new(None),
            chat: Mutex::new(None),
            obs: Mutex::new(None),
            overlay: Mutex::new(live::OverlayStyle::default()),
        };
        be
    }

    fn me(&self) -> Arc<Backend> {
        self.app.state::<Arc<Backend>>().inner().clone()
    }

    /// Send an event to the page.
    pub fn emit(&self, kind: &str, data: Value) {
        let mut m = Map::new();
        m.insert("type".into(), kind.into());
        if let Value::Object(d) = data {
            m.extend(d);
        }
        let _ = self.app.emit("backend", Value::Object(m).to_string());
    }

    /// A call from the page: run it on a worker thread and reply.
    pub fn request(&self, raw: String) {
        let me = self.me();
        std::thread::spawn(move || {
            let msg: Value = serde_json::from_str(&raw).unwrap_or_default();
            let id = msg["id"].clone();
            let method = s(&msg, "method");
            let params = if msg["params"].is_object() { msg["params"].clone() } else { json!({}) };
            if method == "vod.frame" {
                // at most 3 frame grabs at a time (like the Python app's frame pool)
                while me.frames.fetch_add(1, Ordering::SeqCst) >= 3 {
                    me.frames.fetch_sub(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
            }
            let r = me.call(&method, &params);
            if method == "vod.frame" {
                me.frames.fetch_sub(1, Ordering::SeqCst);
            }
            let reply = match r {
                Ok(v) => json!({"id": id, "ok": true, "result": v}),
                Err(e) => {
                    let msg = if e.downcast_ref::<JobError>().is_some() || e.downcast_ref::<UserError>().is_some() {
                        e.to_string()
                    } else if e.downcast_ref::<Cancelled>().is_some() {
                        "Cancelled".into()
                    } else {
                        logs::error(&format!("{method} failed: {e:#}"));
                        format!("{e:#}")
                    };
                    json!({"id": id, "ok": false, "error": msg})
                }
            };
            me.emit("reply", reply);
        });
    }

    fn call(&self, method: &str, p: &Value) -> Result<Value> {
        Ok(match method {
            "app.init" => self.app_init(),
            "app.report" => {
                let st = self.settings.lock().unwrap();
                let extra = [("Model", st.str("model")), ("Method", st.str("method")), ("Translator", st.str("translator"))];
                shell_open(&logs::report_url(&s(p, "what"), &self.gpu.lock().unwrap().clone().unwrap_or_default(), &extra));
                json!(true)
            }
            "app.logs" => {
                let _ = std::fs::create_dir_all(logs::log_dir());
                shell_open(&path_str(&logs::log_dir()));
                json!(true)
            }
            "update.check" => self.check_ytdlp(),
            "appupdate.check" => self.appupdate_check(),
            "appupdate.install" => self.appupdate_install()?,
            "appupdate.rollback" => self.appupdate_rollback()?,
            "components.status" => self.components_status(),
            "components.download" => self.components_download(&s(p, "what"), &s(p, "name")),
            "components.use_gpu_dir" => {
                components::use_existing(&s(p, "dir"), &s(p, "label"))?;
                st_ml::gpu::setup(true);
                self.components_status()
            }
            "components.model_folder" => self.model_folder()?,
            "setup.done" => {
                self.settings.lock().unwrap().save(&json!({"setup_done": true}))?;
                json!(true)
            }
            "model.preload" => {
                self.preload(&s(p, "name"));
                json!(true)
            }
            "settings.save" => Value::Object(self.settings.lock().unwrap().save(&p["data"])?.clone()),
            "source.load" => self.source_load(&s(p, "text"))?,
            "vod.frame" => self.vod_frame(&s(p, "id"), f(p, "t"), p["w"].as_u64().unwrap_or(480) as u32)?,
            "vod.storyboard" => self.vod_storyboard(&s(p, "id"))?,
            "sync.match" => self.sync_match(p)?,
            "snap.run" => self.snap_run(&s(p, "id"), f(p, "lo"), f(p, "hi"))?,
            "snap.cancel" => {
                if let Some(c) = self.snap_cancel.lock().unwrap().as_ref() {
                    c.set();
                }
                json!(true)
            }
            "queue.add" => json!(self.queue_add(p["jobs"].as_array().cloned().unwrap_or_default())),
            "clip.start" => json!(self.queue_add(vec![p.clone()])[0]),
            "queue.cancel" => self.queue_cancel(p.get("id").and_then(|v| v.as_str())),
            "clip.cancel" => {
                let running = self.jobs.lock().unwrap().iter().find(|j| j.status == "running").map(|j| j.id.clone());
                match running {
                    Some(id) => self.queue_cancel(Some(&id)),
                    None => json!(true),
                }
            }
            "queue.clear" => {
                self.jobs.lock().unwrap().retain(|j| j.status == "pending" || j.status == "running");
                self.queue_changed();
                json!(true)
            }
            "subs.open" => self.subs_open(&s(p, "folder"))?,
            "subs.save" => self.subs_save(&s(p, "folder"), &p["cues"])?,
            "subs.burn" => {
                let job = json!({"kind": "burn", "folder": s(p, "folder"), "position": s(p, "position"), "size": s(p, "size"), "label": "Re-burn edited subtitles"});
                json!(self.queue_add(vec![job])[0])
            }
            "history.list" => {
                let items: Vec<Value> = self
                    .history
                    .listing()
                    .into_iter()
                    .map(|mut e| {
                        let t = s(&e, "thumb");
                        e["thumb_url"] = if t.is_empty() { Value::Null } else { file_url(Path::new(&t)).into() };
                        e
                    })
                    .collect();
                json!(items)
            }
            "history.search" => Value::Object(self.history.search(&s(p, "q"), 3)),
            "history.remove" => {
                self.history.remove(&s(p, "id"))?;
                json!(true)
            }
            "glossary.get" => self.glossary.lock().unwrap().summary(),
            "glossary.save" => {
                let mut g = self.glossary.lock().unwrap();
                g.update(&p["data"])?;
                g.summary()
            }
            "glossary.test" => {
                let (fixed, n) = self.glossary.lock().unwrap().fix(&s(p, "text"), true, &[]);
                json!({"fixed": fixed, "changes": n})
            }
            "glossary.slang_test" => self.slang_test(&s(p, "text"), &s(p, "lang"))?,
            "glossary.refresh" => {
                let mut g = self.glossary.lock().unwrap();
                champions::refresh(&mut g, true)?;
                g.summary()
            }
            "os.open" => {
                let path = s(p, "path");
                if !path.is_empty() && Path::new(&path).exists() {
                    shell_open(&path);
                }
                json!(true)
            }
            "os.reveal" => {
                let path = s(p, "path");
                if !path.is_empty() && Path::new(&path).exists() {
                    let _ = st_media::tools::command(Path::new("explorer")).arg(format!("/select,{path}")).spawn();
                }
                json!(true)
            }
            "os.url" => {
                shell_open(&s(p, "url"));
                json!(true)
            }
            "os.pick_video" => rfd::FileDialog::new()
                .set_title("Pick a video to translate")
                .add_filter("Videos", &["mp4", "mkv", "mov", "webm", "avi", "flv", "ts", "m4v", "wmv", "mpg", "mpeg", "3gp"])
                .add_filter("All files", &["*"])
                .pick_file()
                .map(|p| Value::from(path_str(&p)))
                .unwrap_or(Value::Null),
            "os.browse" => {
                let cur = s(p, "current");
                let mut d = rfd::FileDialog::new().set_title("Save translated clips to");
                if !cur.is_empty() {
                    d = d.set_directory(&cur);
                }
                d.pick_folder().map(|p| Value::from(path_str(&p))).unwrap_or(Value::Null)
            }
            "os.copy" => {
                arboard::Clipboard::new().and_then(|mut c| c.set_text(s(p, "text"))).map_err(|e| anyhow!("{e}"))?;
                json!(true)
            }
            "os.clipboard" => json!(arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default()),
            "ui.scale" => self.ui_scale(&p["value"]),
            // live mode comes in Phase 3
            "live.start" => self.live_start(p)?,
            "live.stop" => self.live_stop(),
            "live.update" => self.live_update(p),
            "overlay.show" => self.overlay_show(b(p, "show", true))?,
            "overlay.style" => self.overlay_style(p),
            "obs.url" => self.obs_url()?,
            "chat.toggle" => self.chat_toggle(b(p, "on", true)),
            other => bail!(UserError(format!("unknown method {other}"))),
        })
    }

    // ---------------------------------------------------------------- app

    fn app_init(&self) -> Value {
        let gpu = {
            let mut g = self.gpu.lock().unwrap();
            if g.is_none() {
                *g = Some(components::gpu_name());
            }
            g.clone().unwrap()
        };
        let me = self.me();
        std::thread::spawn(move || {
            let changed = champions::refresh(&mut me.glossary.lock().unwrap(), false).unwrap_or(false);
            if changed {
                let sum = me.glossary.lock().unwrap().summary();
                me.emit("glossary.updated", json!({"glossary": sum}));
            }
        });
        let (auto_update, app_update, preload, model) = {
            let st = self.settings.lock().unwrap();
            (st.bool("auto_update"), st.bool("app_update"), st.bool("preload"), st.str("model"))
        };
        if auto_update {
            let me = self.me();
            std::thread::spawn(move || {
                me.check_ytdlp();
            });
        }
        if app_update {
            let me = self.me();
            std::thread::spawn(move || {
                me.appupdate_check();
            });
        }
        let comp = self.components_status();
        let gpu_ready = comp["gpu"]["ok"].as_bool().unwrap_or(false) || !comp["has_nvidia"].as_bool().unwrap_or(false);
        let model_ok = model == "custom" || comp["models"]["whisper"][&model]["ok"].as_bool().unwrap_or(false);
        let setup_needed = !gpu_ready || !comp["ffmpeg"]["ok"].as_bool().unwrap_or(false) || !model_ok;
        if preload && gpu_ready && !setup_needed {
            let me = self.me();
            std::thread::spawn(move || me.preload(&model));
        }
        let st = self.settings.lock().unwrap().values.clone();
        json!({
            "settings": st, "glossary": self.glossary.lock().unwrap().summary(), "gpu": gpu, "version": VERSION,
            "scale": {"value": *self.zoom.lock().unwrap(), "auto": self.auto_zoom()},
            "update": self.update_info.lock().unwrap().clone(), "app_update": self.app_update.lock().unwrap().clone(),
            "whats_new": self.whats_new(), "crashed": logs::previous_crash(), "ffmpeg": comp["ffmpeg"]["ok"],
            "queue": self.queue_view(), "components": comp, "setup_needed": setup_needed,
            "model": {"name": st.get("model"), "device": self.engines.whisper_device(st.get("model").and_then(|m| m.as_str()).unwrap_or("large-v3"))},
        })
    }

    /// Release notes once, on the first start after an update.
    fn whats_new(&self) -> Value {
        let mut st = self.settings.lock().unwrap();
        let last = st.str("last_version");
        if last == VERSION {
            return Value::Null;
        }
        let _ = st.save(&json!({"last_version": VERSION}));
        if last.is_empty() || !appupdate::is_newer(VERSION, &last) {
            return Value::Null;
        }
        json!({"version": VERSION, "from": last, "notes": appupdate::notes_for(VERSION)})
    }

    fn check_ytdlp(&self) -> Value {
        let info = match st_appkit::ytdlp_update::check_and_update() {
            Ok(mut r) => {
                r["checked"] = py::f(st_appkit::now());
                r
            }
            Err(e) => json!({"current": st_appkit::ytdlp_update::current_version(), "error": e.to_string(), "checked": py::f(st_appkit::now())}),
        };
        *self.update_info.lock().unwrap() = info.clone();
        self.emit("update.status", info.clone());
        info
    }

    fn appupdate_check(&self) -> Value {
        let info = match appupdate::check() {
            Ok(mut r) => {
                r["checked"] = py::f(st_appkit::now());
                r
            }
            Err(e) => json!({"current": VERSION, "error": e.to_string(), "checked": py::f(st_appkit::now())}),
        };
        *self.app_update.lock().unwrap() = info.clone();
        self.emit("appupdate.status", info.clone());
        info
    }

    fn busy(&self) -> bool {
        self.jobs.lock().unwrap().iter().any(|j| j.status == "pending" || j.status == "running") || self.live_running()
    }

    fn appupdate_install(&self) -> Result<Value> {
        let mut info = self.app_update.lock().unwrap().clone();
        if !b(&info, "newer", false) || info["asset"].is_null() {
            info = self.appupdate_check();
        }
        if !b(&info, "newer", false) {
            bail!(UserError("You already have the newest version.".into()));
        }
        self.install_release(&info, VERSION, "")
    }

    /// Go back to the version installed before the last update.
    fn appupdate_rollback(&self) -> Result<Value> {
        let prev = self.settings.lock().unwrap().str("updated_from");
        if prev.is_empty() || !appupdate::is_newer(VERSION, &prev) {
            bail!(UserError("There's no earlier version to go back to.".into()));
        }
        let info = appupdate::release_info(&prev)?;
        if info["asset"].is_null() {
            bail!(UserError(format!("Version {prev}'s installer isn't on GitHub anymore.")));
        }
        logs::info(&format!("Going back from {VERSION} to {prev}"));
        // don't offer this version again right away (newer versions will still be offered)
        self.install_release(&info, "", VERSION)
    }

    fn install_release(&self, info: &Value, updated_from: &str, skip: &str) -> Result<Value> {
        if self.busy() {
            bail!(UserError("Finish or stop the running translations first, then update.".into()));
        }
        if self.updating.swap(true, Ordering::SeqCst) {
            bail!(UserError("The update is already downloading.".into()));
        }
        let r = (|| -> Result<()> {
            let path = appupdate::download(&info["asset"], &|x| self.emit("appupdate.progress", json!({"f": x})), None)?;
            appupdate::save_notes(&s(info, "latest"), &s(info, "notes"))?;
            self.settings.lock().unwrap().save(&json!({"updated_from": updated_from, "skip_version": skip}))?;
            logs::info(&format!("Installing {} (from {VERSION})", s(info, "latest")));
            appupdate::install(&path)
        })();
        self.updating.store(false, Ordering::SeqCst);
        r?;
        self.emit("appupdate.progress", json!({"f": 1, "installing": true}));
        let app = self.app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            app.exit(0);
        });
        Ok(json!({"installing": info["latest"]}))
    }

    fn preload(&self, name: &str) {
        if let Some(d) = self.engines.whisper_device(name) {
            self.emit("model.status", json!({"state": "ready", "name": name, "device": d}));
            return;
        }
        let custom = self.custom_model();
        let first = st_core::models::local_whisper(name, custom.as_deref()).is_none();
        self.emit("model.status", json!({"state": if first { "downloading" } else { "loading" }, "name": name}));
        match self.engines.whisper(name, custom.as_deref(), &|m| logs::info(m), &|_| {}, &Cancel::new()) {
            Ok(m) => self.emit("model.status", json!({"state": "ready", "name": name, "device": m.device})),
            Err(e) => self.emit("model.status", json!({"state": "error", "name": name, "error": e.to_string()})),
        }
    }

    fn custom_model(&self) -> Option<PathBuf> {
        let c = self.settings.lock().unwrap().str("custom_model");
        (!c.is_empty()).then(|| PathBuf::from(c))
    }

    // ---------------------------------------------------------------- setup screen

    fn components_status(&self) -> Value {
        let mut st = components::status();
        let set = self.settings.lock().unwrap();
        st["custom_model"] = set.str("custom_model").into();
        st["selected_model"] = Some(set.str("model")).filter(|m| !m.is_empty()).unwrap_or("large-v3".into()).into();
        st["translator"] = Some(set.str("translator")).filter(|m| !m.is_empty()).unwrap_or("better".into()).into();
        st
    }

    /// what: gpu | whisper | translator | isolation. Runs in the background with progress events.
    fn components_download(&self, what: &str, name: &str) -> Value {
        let me = self.me();
        let (what, name) = (what.to_string(), name.to_string());
        std::thread::spawn(move || {
            let prog = |x: f64, text: &str| me.emit("comp.progress", json!({"what": what, "name": name, "f": (x * 1000.0).round() / 1000.0, "text": text}));
            let cancel = Cancel::new();
            let r = (|| -> Result<()> {
                match what.as_str() {
                    "gpu" => {
                        components::download_gpu_libs(&prog, &cancel)?;
                        st_ml::gpu::setup(true);
                    }
                    "whisper" => {
                        let (repo, gb, label) = st_core::models::WHISPER_MODELS.iter().find(|m| m.0 == name).map(|m| (m.1, m.2, m.3)).ok_or_else(|| anyhow!("unknown model {name}"))?;
                        let title = format!("Whisper {label}");
                        st_media::hub::snapshot(repo, st_core::models::WHISPER_FILES, &|x| prog(x.min(0.99), &format!("Downloading {title} ({:.2} of ~{gb} GB)", x * gb)), &cancel)?;
                        prog(1.0, &format!("{title} ready"));
                    }
                    "translator" => {
                        let (repo, gb) = st_core::models::nllb_repo(&name);
                        st_media::hub::snapshot(repo, st_core::models::NLLB_FILES, &|x| prog(x.min(0.99), &format!("Downloading Translator ({:.2} of ~{gb} GB)", x * gb)), &cancel)?;
                        prog(1.0, "Translator ready");
                    }
                    _ => st_ml::isolate::download_model(&|x| prog(x, "Voice isolation model"))?,
                }
                Ok(())
            })();
            match r {
                Ok(()) => me.emit("comp.done", json!({"what": what, "name": name, "status": me.components_status()})),
                Err(e) => me.emit("comp.error", json!({"what": what, "name": name, "message": e.to_string()})),
            }
        });
        json!(true)
    }

    fn model_folder(&self) -> Result<Value> {
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let Some(d) = rfd::FileDialog::new().set_title("Pick a faster-whisper model folder").set_directory(home).pick_folder() else {
            return Ok(Value::Null);
        };
        let path = components::check_model_folder(&path_str(&d)).map_err(|e| UserError(e.to_string()))?;
        self.settings.lock().unwrap().save(&json!({"custom_model": path, "model": "custom"}))?;
        Ok(self.components_status())
    }

    // ---------------------------------------------------------------- sources, timeline, sync, snap

    fn remember(&self, info: VodInfo) -> VodInfo {
        self.vods.lock().unwrap().insert(info.id.clone(), info.clone());
        info
    }

    fn vod(&self, id: &str) -> Result<VodInfo> {
        self.vods.lock().unwrap().get(id).cloned().ok_or_else(|| JobError("VOD not loaded".into()).into())
    }

    fn source_load(&self, text: &str) -> Result<Value> {
        let (kind, value) = classify(text)?;
        if kind == Kind::Channel {
            return Ok(json!({"kind": "channel", "login": value, "vods": ytdlp::list_vods(&value, 12)?}));
        }
        let mut info = ytdlp::fetch_info(&value)?;
        if info.storyboard.is_none() {
            // a local file's timeline thumbnails, made earlier this session
            info.storyboard = self.vods.lock().unwrap().get(&info.id).and_then(|v| v.storyboard.clone());
        }
        let info = self.remember(info);
        Ok(json!({"kind": "vod", "vod": info.to_dict()}))
    }

    /// Timeline thumbnails for a local video file (Twitch VODs come with their own).
    fn vod_storyboard(&self, id: &str) -> Result<Value> {
        let info = self.vod(id)?;
        if let Some(sb) = info.storyboard {
            return Ok(sb);
        }
        let sb = st_media::local::storyboard(&info)?;
        if let Some(v) = self.vods.lock().unwrap().get_mut(id) {
            v.storyboard = Some(sb.clone());
        }
        Ok(sb)
    }

    fn vod_frame(&self, id: &str, t: f64, w: u32) -> Result<Value> {
        let data = ffmpeg::grab_frame(&self.vod(id)?, t, w)?;
        Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(data)).into())
    }

    /// theirs: one channel/VOD or a list of them; one result per entry.
    fn sync_match(&self, p: &Value) -> Result<Value> {
        let mine = self.remember(ytdlp::fetch_info(&s(p, "mine"))?);
        let (a, z) = (f(p, "my_start"), f(p, "my_end"));
        let targets: Vec<String> = match &p["theirs"] {
            Value::String(t) => vec![t.clone()],
            Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
            _ => vec![],
        };
        let results: Vec<Value> = std::thread::scope(|sc| {
            let hs: Vec<_> = targets
                .chunks(1)
                .map(|t| {
                    let (t, mine) = (t[0].clone(), &mine);
                    sc.spawn(move || match st_job::sync_match(mine, a, z, &t) {
                        Ok((theirs, m)) => {
                            let d = theirs.to_dict();
                            self.remember(theirs);
                            json!({"ok": true, "input": t, "theirs": d, "start": m.start, "end": m.end, "offset": m.offset, "clipped": m.clipped})
                        }
                        Err(e) => json!({"ok": false, "input": t, "error": e.to_string()}),
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().unwrap_or(json!({"ok": false, "error": "crashed"}))).collect()
        });
        Ok(json!({"mine": mine.to_dict(), "results": results}))
    }

    fn snap_run(&self, id: &str, lo: f64, hi: f64) -> Result<Value> {
        let info = self.vod(id)?;
        let c = Cancel::new();
        *self.snap_cancel.lock().unwrap() = Some(c.clone());
        st_vision::snap::find_game(&info, lo, hi, &|x, text| self.emit("snap.progress", json!({"f": (x * 1000.0).round() / 1000.0, "text": text})), &c)
    }

    // ---------------------------------------------------------------- job queue (one job at a time)

    fn queue_view(&self) -> Value {
        json!(self.jobs.lock().unwrap().iter().map(Job::view).collect::<Vec<_>>())
    }

    fn queue_changed(&self) {
        self.emit("queue.update", json!({"jobs": self.queue_view()}));
    }

    fn queue_add(&self, jobs: Vec<Value>) -> Vec<String> {
        let mut ids = vec![];
        {
            let mut q = self.jobs.lock().unwrap();
            for mut p in jobs {
                let kind = p.as_object_mut().and_then(|m| m.remove("kind")).and_then(|k| k.as_str().map(String::from)).unwrap_or("clip".into());
                let id = st_appkit::new_id(8);
                let label = Some(s(&p, "label")).filter(|l| !l.is_empty()).unwrap_or(if kind == "burn" { "Re-burn subtitles".into() } else { "Clip".into() });
                q.push(Job { id: id.clone(), kind, params: p, label, status: "pending".into(), error: None, result: None });
                ids.push(id);
            }
        }
        self.qcond.notify_all();
        self.queue_changed();
        self.ensure_worker();
        ids
    }

    fn queue_cancel(&self, id: Option<&str>) -> Value {
        {
            let mut q = self.jobs.lock().unwrap();
            for j in q.iter_mut().filter(|j| id.is_none_or(|i| j.id == i)) {
                if j.status == "running" {
                    if let Some(c) = self.cancel_current.lock().unwrap().as_ref() {
                        c.set();
                    }
                } else if j.status == "pending" {
                    j.status = "cancelled".into();
                }
            }
        }
        self.queue_changed();
        json!(true)
    }

    fn ensure_worker(&self) {
        static STARTED: AtomicBool = AtomicBool::new(false);
        if !STARTED.swap(true, Ordering::SeqCst) {
            let me = self.me();
            std::thread::Builder::new().name("queue".into()).spawn(move || me.queue_worker()).unwrap();
        }
    }

    fn queue_worker(&self) {
        let mut batch_done = 0;
        loop {
            let (jid, kind, params, label, cancel) = {
                let mut q = self.jobs.lock().unwrap();
                while !q.iter().any(|j| j.status == "pending") {
                    if batch_done > 1 {
                        self.notify("All translations finished", &format!("{batch_done} jobs are done."));
                    }
                    batch_done = 0;
                    q = self.qcond.wait(q).unwrap();
                }
                let j = q.iter_mut().find(|j| j.status == "pending").unwrap();
                j.status = "running".into();
                let c = Cancel::new();
                *self.cancel_current.lock().unwrap() = Some(c.clone());
                (j.id.clone(), j.kind.clone(), j.params.clone(), j.label.clone(), c)
            };
            self.queue_changed();
            let (me1, me2, me3, me4) = (self.me(), self.me(), self.me(), self.me());
            let (j1, j2, j3, j4, l3) = (jid.clone(), jid.clone(), jid.clone(), jid.clone(), label.clone());
            let cb = Callbacks {
                stage: Box::new(move |i, n, l| me1.emit("clip.stage", json!({"job": j1, "i": i, "n": n, "label": l}))),
                progress: Box::new(move |x| me2.emit("clip.progress", json!({"job": j2, "f": (x * 10000.0).round() / 10000.0}))),
                log: Box::new(move |t| {
                    logs::info(&format!("[{l3}] {t}"));
                    me3.emit("clip.log", json!({"job": j3, "text": t}));
                }),
                segment: Box::new(move |seg, k| me4.emit("clip.segment", json!({"job": j4, "start": seg.start, "end": seg.end, "text": seg.text, "kind": k}))),
                cancel,
            };
            let mut shown = params.clone();
            if let Some(m) = shown.as_object_mut() {
                m.remove("out_dir");
                m.remove("champions");
            }
            logs::info(&format!("Job started: {label} ({kind}) {shown}"));
            let r = if kind == "clip" { self.run_clip(&params, &label, &cb) } else { self.run_burn(&params, &cb) };
            let (status, error, result) = match r {
                Ok(res) => {
                    logs::info(&format!("Job finished: {label}"));
                    batch_done += 1;
                    let mut ev = json!({"job": jid, "kind": kind});
                    if let (Some(e), Some(rm)) = (ev.as_object_mut(), res.as_object()) {
                        e.extend(rm.clone());
                    }
                    self.emit("clip.done", ev);
                    ("done", None, Some(res))
                }
                Err(e) if e.downcast_ref::<Cancelled>().is_some() => {
                    self.emit("clip.cancelled", json!({"job": jid}));
                    ("cancelled", None, None)
                }
                Err(e) if e.downcast_ref::<JobError>().is_some() => {
                    let m = e.to_string();
                    logs::warning(&format!("Job failed: {label}: {m}"));
                    self.emit("clip.error", json!({"job": jid, "message": m}));
                    self.notify("Translation failed", &format!("{label}: {}", m.lines().next().unwrap_or("")));
                    ("error", Some(m), None)
                }
                Err(e) => {
                    logs::error(&format!("Job crashed: {label}: {e:#}"));
                    let m = format!("Something went wrong: {e:#}\n\nThe details are in the log. Use Report to send them.");
                    self.emit("clip.error", json!({"job": jid, "message": m, "report": true}));
                    self.notify("Translation failed", &format!("{label}: {e}"));
                    ("error", Some(m), None)
                }
            };
            {
                let mut q = self.jobs.lock().unwrap();
                if let Some(j) = q.iter_mut().find(|j| j.id == jid) {
                    j.status = status.into();
                    j.error = error;
                    j.result = result;
                }
            }
            self.queue_changed();
        }
    }

    fn run_clip(&self, p: &Value, label: &str, cb: &Callbacks) -> Result<Value> {
        let custom = self.custom_model(); // takes the settings lock itself, so not inside the block below
        let (out_dir, translator) = {
            let st = self.settings.lock().unwrap();
            (Some(s(p, "out_dir")).filter(|d| !d.is_empty()).unwrap_or(st.str("out_dir")), Some(st.str("translator")).filter(|t| !t.is_empty()).unwrap_or("better".into()))
        };
        let or = |k: &str, d: &str| Some(s(p, k)).filter(|v| !v.is_empty()).unwrap_or(d.into());
        let params = JobParams {
            url: s(p, "url"),
            start: f(p, "start"),
            end: f(p, "end"),
            out_dir: PathBuf::from(out_dir),
            quality: or("quality", "720p"),
            model: or("model", "large-v3"),
            language: Some(s(p, "language")).filter(|l| !l.is_empty()),
            want_original: b(p, "orig", false),
            burn: b(p, "burn", true),
            position: or("position", "Above game HUD"),
            font_size: or("size", "Medium"),
            champions: p["champions"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            use_glossary: b(p, "use_glossary", true),
            isolate: b(p, "isolate", false),
            label: label.into(),
            method: or("method", "two-step"),
            translator,
            auto_details: b(p, "auto_details", true),
            speakers: b(p, "speakers", false),
            custom_model: custom,
        };
        let r = run_job(&params, cb, &self.engines)?;
        self.emit("model.status", json!({"state": "ready", "name": params.model, "device": self.engines.whisper_device(&params.model)}));
        let m = &r.meta;
        let thumb = m["thumb"].as_str().map(|t| path_str(&r.folder.join(t)));
        let mut entry = Map::new();
        for (k, v) in [
            ("type", json!("clip")), ("folder", json!(path_str(&r.folder))), ("title", m["title"].clone()), ("channel", m["channel"].clone()),
            ("vod_url", m["vod_url"].clone()), ("vod_id", m["vod_id"].clone()), ("start", m["start"].clone()), ("end", m["end"].clone()),
            ("language", json!(r.language)), ("language_name", json!(lang_name(Some(&r.language)))), ("lines", json!(r.segments)),
            ("thumb", json!(thumb)), ("clip", json!(path_str(&r.clip))), ("srt", json!(path_str(&r.srt))), ("transcript", json!(path_str(&r.transcript))),
            ("subbed", json!(r.subbed.as_deref().map(path_str))), ("seconds", py::f(r.seconds)),
        ] {
            entry.insert(k.into(), v);
        }
        let e = self.history.add(entry)?;
        self.emit("history.changed", json!({}));
        self.notify("Translation finished", &format!("{}: {} lines from {}", s(m, "channel"), r.segments, lang_name(Some(&r.language))));
        Ok(json!({
            "folder": path_str(&r.folder), "clip": path_str(&r.clip), "srt": path_str(&r.srt), "transcript": path_str(&r.transcript),
            "subbed": r.subbed.as_deref().map(path_str), "language": r.language, "language_name": lang_name(Some(&r.language)),
            "probability": r.probability, "segments": r.segments, "seconds": r.seconds, "history_id": e["id"], "channel": m["channel"],
        }))
    }

    fn run_burn(&self, p: &Value, cb: &Callbacks) -> Result<Value> {
        let t0 = std::time::Instant::now();
        let folder = s(p, "folder");
        let out = reburn(Path::new(&folder), &Some(s(p, "position")).filter(|x| !x.is_empty()).unwrap_or("Above game HUD".into()), &Some(s(p, "size")).filter(|x| !x.is_empty()).unwrap_or("Medium".into()), cb)?;
        for e in self.history.listing() {
            if s(&e, "folder") == folder {
                self.history.update(&s(&e, "id"), &json!({"subbed": path_str(&out)}))?;
            }
        }
        self.emit("history.changed", json!({}));
        self.notify("Subtitles burned", "The edited video is ready.");
        Ok(json!({"folder": folder, "subbed": path_str(&out), "seconds": t0.elapsed().as_secs_f64()}))
    }

    // ---------------------------------------------------------------- subtitle editor

    /// WebView2 plays H.264 and AAC directly, so the editor uses the clip itself (no WebM preview copy).
    fn subs_open(&self, folder: &str) -> Result<Value> {
        let fp = Path::new(folder);
        let meta = read_meta(fp)?;
        let cues = read_srt(&fp.join(s(&meta, "srt")))?;
        let clip = fp.join(s(&meta, "clip"));
        if !clip.exists() {
            bail!(JobError("The clip file for these subtitles is missing.".into()));
        }
        let poster = meta["thumb"].as_str().and_then(|t| file_url(&fp.join(t)));
        Ok(json!({"cues": cues, "video": file_url(&clip), "meta": meta, "folder": folder, "poster": poster}))
    }

    fn subs_save(&self, folder: &str, cues: &Value) -> Result<Value> {
        let fp = Path::new(folder);
        let mut meta = read_meta(fp)?;
        let srt = fp.join(s(&meta, "srt"));
        let backup = srt.with_file_name(format!("{}.original.srt", srt.file_stem().unwrap().to_string_lossy()));
        if !backup.exists() && srt.exists() {
            std::fs::copy(&srt, &backup)?;
        }
        let cues: Vec<Cue> = serde_json::from_value(cues.clone())?;
        write_srt_cues(&cues, &srt)?;
        let lines = cues.iter().filter(|c| !c.text.trim().is_empty()).count();
        meta["edited"] = py::f(st_appkit::now());
        meta["lines"] = lines.into();
        write_meta(fp, &meta)?;
        for e in self.history.listing() {
            if s(&e, "folder") == folder {
                self.history.update(&s(&e, "id"), &json!({"lines": lines, "edited": meta["edited"]}))?;
            }
        }
        self.emit("history.changed", json!({}));
        Ok(json!({"saved": path_str(&srt), "backup": path_str(&backup)}))
    }

    // ---------------------------------------------------------------- glossary

    fn slang_test(&self, text: &str, lang: &str) -> Result<Value> {
        if nllb_code(lang).is_none() {
            bail!(UserError("The text translator doesn't cover that language.".into()));
        }
        let (helped, n) = self.glossary.lock().unwrap().prepare(text, Some(lang));
        let size = self.settings.lock().unwrap().str("translator");
        let tr = self.engines.translator(&size, &|m| logs::info(m), &|_| {}, &Cancel::new())?;
        let out = tr.translate(&[text.to_string(), helped], lang, 4)?;
        let g = self.glossary.lock().unwrap();
        Ok(json!({"plain": g.fix(&out[0], true, &[]).0, "helped": g.fix(&out[1], true, &[]).0, "changes": n}))
    }

    // ---------------------------------------------------------------- window

    fn auto_zoom(&self) -> f64 {
        self.app
            .get_webview_window("main")
            .and_then(|w| w.current_monitor().ok().flatten())
            .map(|m| auto_scale(m.work_area().size.height as f64 / m.scale_factor()))
            .unwrap_or(1.0)
    }

    /// Apply the saved interface size ("auto" or a number).
    pub fn apply_scale(&self) -> f64 {
        let v = self.settings.lock().unwrap().get("ui_scale");
        let z = match &v {
            Value::Number(n) => n.as_f64().unwrap_or(1.0),
            Value::String(x) => x.parse::<f64>().unwrap_or_else(|_| self.auto_zoom()),
            _ => self.auto_zoom(),
        };
        if let Some(w) = self.app.get_webview_window("main") {
            let _ = w.set_zoom(z);
        }
        *self.zoom.lock().unwrap() = z;
        z
    }

    /// value: "auto", a number like 1.5, or "+" / "-" / "0" from the keyboard shortcuts.
    fn ui_scale(&self, value: &Value) -> Value {
        let cur = *self.zoom.lock().unwrap();
        let setting = match value.as_str() {
            Some("+") => json!((((cur + 0.1) * 20.0).round_ties_even() / 20.0).min(3.0)),
            Some("-") => json!((((cur - 0.1) * 20.0).round_ties_even() / 20.0).max(0.5)),
            Some("0") => json!("auto"),
            _ => value.clone(),
        };
        {
            let mut st = self.settings.lock().unwrap();
            st.values.insert("ui_scale".into(), setting.clone());
            let _ = st.save(&json!({}));
        }
        let z = self.apply_scale();
        self.emit("ui.scale", json!({"value": z, "setting": setting, "auto": self.auto_zoom()}));
        json!(z)
    }

    /// A Windows notification, only when the window isn't in front.
    fn notify(&self, title: &str, text: &str) {
        if !self.settings.lock().unwrap().bool("notify") {
            return;
        }
        if let Some(w) = self.app.get_webview_window("main") {
            if w.is_focused().unwrap_or(false) && !w.is_minimized().unwrap_or(false) {
                return; // they're looking at the app already
            }
            let _ = w.request_user_attention(Some(tauri::UserAttentionType::Informational));
        }
        let _ = self.app.notification().builder().title(title).body(text).show();
    }

    pub fn shutdown(&self) {
        self.queue_cancel(None);
        self.live_stop();
    }
}

/// An error whose message is meant for the user as is (Python's ValueError in the backend).
#[derive(Debug)]
pub struct UserError(pub String);

impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for UserError {}
