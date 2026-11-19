//! Live mode in the backend (app.py m_live_*, m_chat_toggle, m_overlay_*, m_obs_url): starts and stops
//! the live session and chat, mirrors captions into the floating caption window and the OBS source,
//! and saves finished sessions to History.

use std::sync::{Arc, Mutex, Weak};

use anyhow::Result;
use serde_json::{json, Map, Value};
use st_core::sync::{classify, Kind};
use st_live::{
    chat::ChatSession,
    obs::ObsServer,
    team::{self, TeamOptions, TeamSession, Voice},
    Emit, LiveOptions, LiveSession,
};
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use super::{b, s, Backend};

/// The floating caption window's look (overlay.py CaptionOverlay).
pub struct OverlayStyle {
    pub font_px: f64,
    pub bg_alpha: f64,
    pub show_orig: bool,
    /// the app's interface size when the window was made
    pub scale: f64,
    pub width: f64,
}

impl Default for OverlayStyle {
    fn default() -> Self {
        OverlayStyle { font_px: 30.0, bg_alpha: 0.55, show_orig: true, scale: 1.0, width: 900.0 }
    }
}

/// The caption window's page, served from the app (http://overlay.localhost/).
pub const OVERLAY_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>StreamTranslator captions</title>
<style>
html,body{margin:0;height:100%;background:transparent;overflow:hidden;font-family:'Segoe UI',system-ui,sans-serif;user-select:none;cursor:move}
#box{position:absolute;inset:2px;border-radius:14px;display:flex;flex-direction:column;justify-content:flex-end;align-items:center;
 gap:6px;padding:12px 20px;box-sizing:border-box;border:1.5px solid transparent}
#box.on{background:rgba(8,8,12,var(--bg,.55))}
body:hover #box{background:rgba(8,8,12,var(--bg,.55));border-color:rgba(145,70,255,.78)}
#hint{position:absolute;top:6px;right:12px;font-size:var(--hint,12px);color:#c8c8d2;display:none}
body:hover #hint{display:block}
.t{text-align:center;font-weight:700;color:#fff;font-size:var(--size,30px);line-height:1.25;
 text-shadow:-1.5px 0 #000c,1.5px 0 #000c,0 -1.5px #000c,0 1.5px #000c;overflow-wrap:anywhere}
.t.partial{color:rgba(255,255,255,.67)}
.t.team{font-size:calc(var(--size,30px)*.85)} .who{font-weight:800}
.o{text-align:center;font-weight:400;color:rgba(190,190,205,.86);font-size:max(12px,calc(var(--size,30px)*.55));
 text-shadow:-1.5px 0 #000c,1.5px 0 #000c,0 -1.5px #000c,0 1.5px #000c}
</style></head><body>
<div id="box" data-tauri-drag-region><div id="hint">drag to move · scroll: text size · ctrl+scroll: width · right-click: hide</div></div>
<script>
const T = window.__TAURI__, box = document.getElementById('box'), hint = document.getElementById('hint'), root = document.documentElement;
let cap = { text: 'Captions will appear here', orig: '', partial: false }, showOrig = true, fade;
function render() {
  box.querySelectorAll('.t,.o').forEach(e => e.remove());
  box.classList.toggle('on', !!cap.text);
  if (cap.lines) {
    cap.lines.slice(-3).forEach(l => { const d = document.createElement('div'); d.className = 't team'; const n = document.createElement('span'); n.className = 'who';
      n.style.color = l.color; n.textContent = l.name + ': '; d.append(n, l.text); box.appendChild(d); });
    return;
  }
  if (cap.text) { const d = document.createElement('div'); d.className = 't' + (cap.partial ? ' partial' : ''); d.textContent = cap.text; box.appendChild(d); }
  if (cap.orig && showOrig) { const o = document.createElement('div'); o.className = 'o'; o.textContent = cap.orig; box.appendChild(o); }
}
T.event.listen('overlay-caption', e => {
  const m = e.payload;
  if (m.partial && !m.text) return;
  cap = m; render();
  clearTimeout(fade); fade = setTimeout(() => { cap = { text: '', orig: '' }; render(); }, 12000);
});
T.event.listen('overlay-style', e => {
  const m = e.payload;
  root.style.setProperty('--size', m.size + 'px'); root.style.setProperty('--bg', m.bg); root.style.setProperty('--hint', m.hint + 'px');
  showOrig = m.orig; render();
});
document.addEventListener('mousedown', e => { if (e.button === 0) T.window.getCurrentWindow().startDragging(); });
document.addEventListener('contextmenu', e => { e.preventDefault(); T.core.invoke('overlay_cmd', { action: 'hide', up: false, ctrl: false }); });
document.addEventListener('wheel', e => { e.preventDefault(); T.core.invoke('overlay_cmd', { action: 'wheel', up: e.deltaY < 0, ctrl: e.ctrlKey }); }, { passive: false });
render();
T.core.invoke('overlay_cmd', { action: 'ready', up: false, ctrl: false });
</script></body></html>"#;

impl Backend {
    // ---------------------------------------------------------------- live session

    pub(super) fn live_start(&self, p: &Value) -> Result<Value> {
        self.live_stop();
        let (save_dir, translator, custom) = {
            let st = self.settings.lock().unwrap();
            (st.str("out_dir"), Some(st.str("translator")).filter(|t| !t.is_empty()).unwrap_or("better".into()), st.str("custom_model"))
        };
        let opts = LiveOptions {
            source: s(p, "source"),
            model: Some(s(p, "model")).filter(|m| !m.is_empty()).unwrap_or("large-v3".into()),
            custom_model: Some(custom).filter(|c| !c.is_empty()).map(Into::into),
            language: Some(s(p, "language")).filter(|l| !l.is_empty()),
            show_original: b(p, "orig", true),
            use_glossary: b(p, "use_glossary", true),
            champions: p["champions"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            isolate: b(p, "isolate", false),
            method: Some(s(p, "method")).filter(|m| !m.is_empty()).unwrap_or("two-step".into()),
            translator,
            save_dir: save_dir.into(),
        };
        // the session's events go through here; it needs to know which session sent them
        let slot: Arc<Mutex<Weak<LiveSession>>> = Arc::new(Mutex::new(Weak::new()));
        let (app, sl) = (self.app.clone(), slot.clone());
        let emit: Emit = Arc::new(move |kind: &str, data: Value| {
            let be = app.state::<Arc<Backend>>();
            be.live_event(sl.lock().unwrap().upgrade().as_deref(), kind, data);
        });
        let live = LiveSession::new(opts, self.engines.clone(), self.glossary.clone(), emit);
        *slot.lock().unwrap() = Arc::downgrade(&live);
        *self.live.lock().unwrap() = Some(live.clone());
        live.start();
        if b(p, "chat", true) {
            match classify(&s(p, "source")) {
                Ok((Kind::Channel, login)) => self.start_chat(&login),
                _ => self.emit("chat.status", json!({"state": "unavailable", "message": "Chat translation works for Twitch channels."})),
            }
        }
        Ok(json!(true))
    }

    fn live_event(&self, live: Option<&LiveSession>, kind: &str, data: Value) {
        // mirror captions into the caption window and the OBS source
        let caption = match kind {
            "live.caption" => Some(json!({"text": data["text"], "orig": data.get("orig").cloned().unwrap_or(json!("")), "partial": false})),
            "live.partial" => Some(json!({"text": data["text"], "orig": "", "partial": true})),
            _ => None,
        };
        let stopped = kind == "live.status" && data["state"] == "stopped";
        let fatal_error = (kind == "live.error" && data.get("fatal") != Some(&json!(false))).then(|| s(&data, "message"));
        self.emit(kind, data);
        if let Some(c) = caption {
            if self.app.get_webview_window("overlay").is_some_and(|w| w.is_visible().unwrap_or(false)) {
                let _ = self.app.emit("overlay-caption", c.clone());
            }
            if let Some(obs) = self.obs.lock().unwrap().as_ref() {
                obs.send(&c);
            }
        }
        if stopped {
            if let Some(live) = live {
                self.save_live(live);
            }
        }
        if let Some(m) = fatal_error {
            self.notify("Live translation stopped", &m);
        }
    }

    /// A finished session goes to History (only when it produced captions).
    fn save_live(&self, live: &LiveSession) {
        use std::sync::atomic::Ordering;
        let Some(transcript) = live.transcript_path.lock().unwrap().clone() else { return };
        let lines = live.lines.load(Ordering::SeqCst);
        if lines == 0 {
            return;
        }
        let info = live.info.lock().unwrap().clone();
        let path = |p: Option<std::path::PathBuf>| p.map(|p| Value::from(p.to_string_lossy().into_owned())).unwrap_or(Value::Null);
        let mut e = Map::new();
        for (k, v) in [
            ("type", json!("live")), ("channel", json!(info.channel)), ("title", json!(info.title)), ("url", json!(info.url)),
            ("transcript", path(Some(transcript))), ("srt", path(live.srt_path.lock().unwrap().clone())),
            ("thumb", path(live.thumb_path.lock().unwrap().clone())), ("lines", json!(lines)),
            ("duration", st_core::py::f(live.started.elapsed().as_secs_f64())),
        ] {
            e.insert(k.into(), v);
        }
        match self.history.add(e) {
            Ok(_) => self.emit("history.changed", json!({})),
            Err(err) => st_appkit::logs::warning(&format!("Couldn't save the live session to History: {err}")),
        }
    }

    pub(super) fn live_stop(&self) -> Value {
        if let Some(l) = self.live.lock().unwrap().as_ref() {
            l.stop();
        }
        if let Some(c) = self.chat.lock().unwrap().take() {
            c.stop();
        }
        json!(true)
    }

    pub(super) fn live_update(&self, p: &Value) -> Value {
        if let Some(l) = self.live.lock().unwrap().as_ref() {
            l.update(p);
        }
        json!(true)
    }

    pub(super) fn live_running(&self) -> bool {
        self.live.lock().unwrap().as_ref().is_some_and(|l| !l.is_stopped()) || self.team.lock().unwrap().as_ref().is_some_and(|t| !t.is_stopped())
    }

    // ---------------------------------------------------------------- team voice

    pub(super) fn team_start(&self, p: &Value) -> Result<Value> {
        self.team_stop();
        self.live_stop();
        let (save_dir, custom) = {
            let st = self.settings.lock().unwrap();
            (st.str("out_dir"), st.str("custom_model"))
        };
        let dev = |k: &str| Some(s(p, k)).filter(|d| !d.is_empty());
        let opts = TeamOptions {
            team_device: dev("team_device"),
            mic_device: dev("mic_device"),
            my_name: Some(s(p, "my_name").trim().to_string()).filter(|n| !n.is_empty()).unwrap_or("Me".into()),
            model: Some(s(p, "model")).filter(|m| !m.is_empty()).unwrap_or("large-v3".into()),
            custom_model: Some(custom).filter(|c| !c.is_empty()).map(Into::into),
            language: Some(s(p, "language")).filter(|l| !l.is_empty()),
            use_glossary: b(p, "use_glossary", true),
            champions: p["champions"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
            save_dir: save_dir.into(),
            voices: team::load_voices(),
            discord: None,
        };
        let mut opts = opts;
        if s(p, "source") == "discord" {
            let st = self.settings.lock().unwrap();
            let id = |k: &str| st.str(k).trim().parse::<u64>().ok();
            let token = st.str("discord_token").trim().to_string();
            match (token.is_empty(), id("discord_guild"), id("discord_channel")) {
                (false, Some(guild), Some(channel)) => opts.discord = Some(st_live::discord::DiscordTarget { token, guild, channel }),
                (true, ..) => return Err(st_core::JobError("Paste your Discord bot token first.".into()).into()),
                _ => return Err(st_core::JobError("Pick the Discord voice channel first.".into()).into()),
            }
            opts.team_device = None;
            opts.mic_device = None;
        }
        let slot: Arc<Mutex<Weak<TeamSession>>> = Arc::new(Mutex::new(Weak::new()));
        let (app, sl) = (self.app.clone(), slot.clone());
        let emit: Emit = Arc::new(move |kind: &str, data: Value| {
            let be = app.state::<Arc<Backend>>();
            be.team_event(sl.lock().unwrap().upgrade().as_deref(), kind, data);
        });
        let t = TeamSession::new(opts, self.engines.clone(), self.glossary.clone(), emit);
        *slot.lock().unwrap() = Arc::downgrade(&t);
        self.team_recent.lock().unwrap().clear();
        *self.team.lock().unwrap() = Some(t.clone());
        t.start();
        Ok(json!(true))
    }

    pub(super) fn team_stop(&self) -> Value {
        if let Some(t) = self.team.lock().unwrap().as_ref() {
            t.stop();
        }
        json!(true)
    }

    pub(super) fn team_update(&self, p: &Value) -> Value {
        if let Some(t) = self.team.lock().unwrap().as_ref() {
            t.update(p);
        }
        json!(true)
    }

    fn team_event(&self, team: Option<&TeamSession>, kind: &str, data: Value) {
        let stopped = kind == "team.status" && data["state"] == "stopped";
        let fatal_error = (kind == "team.error" && data.get("fatal") != Some(&json!(false))).then(|| s(&data, "message"));
        if kind == "team.caption" && data["hidden"] != json!(true) {
            // the caption window and OBS show the last few lines, each with its speaker
            let lines = {
                let mut r = self.team_recent.lock().unwrap();
                r.push(json!({"id": data["id"], "name": data["name"], "color": data["color"], "text": data["text"], "avatar": data["avatar"]}));
                let extra = r.len().saturating_sub(6);
                r.drain(..extra);
                r.clone()
            };
            let c = json!({"text": data["text"], "orig": "", "partial": false, "lines": lines});
            if self.app.get_webview_window("overlay").is_some_and(|w| w.is_visible().unwrap_or(false)) {
                let _ = self.app.emit("overlay-caption", c.clone());
            }
            if let Some(obs) = self.obs.lock().unwrap().as_ref() {
                obs.send(&c);
            }
        }
        self.emit(kind, data);
        if stopped {
            if let Some(t) = team {
                self.save_team(t);
            }
        }
        if let Some(m) = fatal_error {
            self.notify("Team voice stopped", &m);
        }
    }

    fn save_team(&self, t: &TeamSession) {
        use std::sync::atomic::Ordering;
        let Some(transcript) = t.transcript_path.lock().unwrap().clone() else { return };
        let lines = t.lines.load(Ordering::SeqCst);
        if lines == 0 {
            return;
        }
        let mut e = Map::new();
        for (k, v) in [
            ("type", json!("live")), ("channel", json!("Team voice")), ("title", json!("Team voice call")), ("url", json!("")),
            ("transcript", json!(transcript.to_string_lossy())), ("srt", Value::Null), ("thumb", Value::Null), ("lines", json!(lines)),
            ("duration", st_core::py::f(t.started.elapsed().as_secs_f64())),
        ] {
            e.insert(k.into(), v);
        }
        match self.history.add(e) {
            Ok(_) => self.emit("history.changed", json!({})),
            Err(err) => st_appkit::logs::warning(&format!("Couldn't save the team session to History: {err}")),
        }
    }

    /// Listen to one teammate for ~10 s and save their voice under `name` (replacing an earlier one).
    pub(super) fn team_learn(&self, p: &Value) -> Result<Value> {
        let name = s(p, "name").trim().to_string();
        if name.is_empty() {
            return Err(st_core::JobError("Type the teammate's name first.".into()).into());
        }
        let device = s(p, "device");
        if device.is_empty() {
            return Err(st_core::JobError("Pick the device your team call plays on first.".into()).into());
        }
        let cancel = st_core::cancel::Cancel::new();
        *self.team_learn.lock().unwrap() = Some(cancel.clone());
        let r = team::learn_voice(&device, &|f, level| self.emit("team.learn", json!({"name": name, "f": (f * 1000.0).round() / 1000.0, "level": level})), &cancel);
        *self.team_learn.lock().unwrap() = None;
        let emb = r?;
        let mut voices = team::load_voices();
        let color = match voices.iter().find(|v| v.name.eq_ignore_ascii_case(&name)) {
            Some(v) => v.color.clone(),
            None => team::PALETTE.iter().find(|c| !voices.iter().any(|v| v.color == **c)).unwrap_or(&team::PALETTE[voices.len() % team::PALETTE.len()]).to_string(),
        };
        voices.retain(|v| !v.name.eq_ignore_ascii_case(&name));
        voices.push(Voice { name, color, emb, hidden: false });
        team::save_voices(&voices)?;
        if let Some(t) = self.team.lock().unwrap().as_ref() {
            t.set_voices(voices.clone());
        }
        Ok(team::voices_view(&voices))
    }

    pub(super) fn team_learn_cancel(&self) -> Value {
        if let Some(c) = self.team_learn.lock().unwrap().as_ref() {
            c.set();
        }
        json!(true)
    }

    /// Rename a learned voice. New lines use the new name; the lines still on screen (caption window and
    /// OBS) are updated too.
    pub(super) fn team_rename(&self, name: &str, to: &str) -> Result<Value> {
        let to = to.trim();
        if to.is_empty() {
            return Err(st_core::JobError("Type a name.".into()).into());
        }
        let mut voices = team::load_voices();
        if voices.iter().any(|v| v.name.eq_ignore_ascii_case(to) && v.name != name) {
            return Err(st_core::JobError(format!("There's already a teammate called {to}.")).into());
        }
        for v in voices.iter_mut().filter(|v| v.name == name) {
            v.name = to.to_string();
        }
        team::save_voices(&voices)?;
        if let Some(t) = self.team.lock().unwrap().as_ref() {
            t.set_voices(voices.clone());
        }
        self.relabel_recent(name, to);
        Ok(team::voices_view(&voices))
    }

    /// The lines still on screen (caption window and OBS) under a new name.
    fn relabel_recent(&self, name: &str, to: &str) {
        let lines = {
            let mut r = self.team_recent.lock().unwrap();
            for l in r.iter_mut().filter(|l| l["name"] == name) {
                l["name"] = json!(to);
            }
            r.clone()
        };
        if let Some(last) = lines.last() {
            let c = json!({"text": last["text"], "orig": "", "partial": false, "lines": lines});
            if self.app.get_webview_window("overlay").is_some_and(|w| w.is_visible().unwrap_or(false)) {
                let _ = self.app.emit("overlay-caption", c.clone());
            }
            if let Some(obs) = self.obs.lock().unwrap().as_ref() {
                obs.send(&c);
            }
        }
    }

    /// Rename someone heard through Discord (an empty name goes back to their Discord name).
    pub(super) fn team_rename_person(&self, id: &str, to: &str) -> Result<Value> {
        let to = to.trim();
        let mut people = team::load_people();
        let Some(i) = people.iter().position(|x| x.id == id) else {
            return Err(st_core::JobError("That person isn't known yet.".into()).into());
        };
        let old = people[i].shown().to_string();
        let new = if to.is_empty() { people[i].discord.clone() } else { to.to_string() };
        if people.iter().enumerate().any(|(j, x)| j != i && x.shown().eq_ignore_ascii_case(&new)) {
            return Err(st_core::JobError(format!("Someone else is already called {new}.")).into());
        }
        people[i].name = (new != people[i].discord).then(|| new.clone());
        team::save_people(&people)?;
        if let Some(t) = self.team.lock().unwrap().as_ref() {
            t.set_people(people.clone());
        }
        self.relabel_recent(&old, &new);
        Ok(team::person_view(&people[i]))
    }

    /// Keep someone off stream (caption window, OBS, subtitles), or show them again. `id`: a Discord
    /// person; `name`: a learned voice. Their lines already on screen go at once.
    pub(super) fn team_hide(&self, p: &Value) -> Result<Value> {
        let hidden = b(p, "hidden", true);
        let (id, name) = (s(p, "id"), s(p, "name"));
        let (shown_name, view) = if !id.is_empty() {
            let mut people = team::load_people();
            let Some(x) = people.iter_mut().find(|x| x.id == id) else { return Err(st_core::JobError("That person isn't known yet.".into()).into()) };
            x.hidden = hidden;
            let (n, v) = (x.shown().to_string(), team::person_view(x));
            team::save_people(&people)?;
            if let Some(t) = self.team.lock().unwrap().as_ref() {
                t.set_people(people.clone());
            }
            (n, v)
        } else {
            let mut voices = team::load_voices();
            for v in voices.iter_mut().filter(|v| v.name == name) {
                v.hidden = hidden;
            }
            team::save_voices(&voices)?;
            if let Some(t) = self.team.lock().unwrap().as_ref() {
                t.set_voices(voices.clone());
            }
            (name.clone(), team::voices_view(&voices))
        };
        if hidden {
            let lines = {
                let mut r = self.team_recent.lock().unwrap();
                r.retain(|l| l["name"] != json!(shown_name));
                r.clone()
            };
            let c = json!({"text": lines.last().map(|l| l["text"].clone()).unwrap_or(json!("")), "orig": "", "partial": false, "lines": lines});
            if self.app.get_webview_window("overlay").is_some_and(|w| w.is_visible().unwrap_or(false)) {
                let _ = self.app.emit("overlay-caption", c.clone());
            }
            if let Some(obs) = self.obs.lock().unwrap().as_ref() {
                obs.send(&c);
            }
        }
        Ok(view)
    }

    /// A word Whisper keeps getting wrong, fixed from a caption: added to the glossary's fixes (and the
    /// right spelling to its custom words, so Whisper hears it as a hint too).
    pub(super) fn glossary_add_fix(&self, wrong: &str, right: &str) -> Result<Value> {
        let (wrong, right) = (wrong.trim(), right.trim());
        if wrong.is_empty() || right.is_empty() || wrong == right {
            return Err(st_core::JobError("Type what it should say.".into()).into());
        }
        let mut g = self.glossary.lock().unwrap();
        g.fixes.retain(|(k, _)| !k.eq_ignore_ascii_case(wrong));
        g.fixes.push((wrong.to_string(), right.to_string()));
        if right.split_whitespace().count() <= 3 && !g.custom_words.iter().any(|w| w.eq_ignore_ascii_case(right)) {
            g.custom_words.push(right.to_string());
        }
        g.save()?;
        let sum = g.summary();
        drop(g);
        self.emit("glossary.updated", json!({"glossary": sum.clone()}));
        Ok(sum)
    }

    /// The caption look in OBS: saved, and sent to open browser sources at once.
    pub(super) fn obs_style(&self, style: &Value) -> Result<Value> {
        self.settings.lock().unwrap().save(&json!({"obs_style": style}))?;
        if let Some(obs) = self.obs.lock().unwrap().as_ref() {
            obs.set_style(style);
        }
        Ok(json!(true))
    }

    pub(super) fn team_forget(&self, name: &str) -> Result<Value> {
        let mut voices = team::load_voices();
        voices.retain(|v| v.name != name);
        team::save_voices(&voices)?;
        if let Some(t) = self.team.lock().unwrap().as_ref() {
            t.set_voices(voices.clone());
        }
        Ok(team::voices_view(&voices))
    }

    // ---------------------------------------------------------------- chat

    fn start_chat(&self, channel: &str) {
        if let Some(c) = self.chat.lock().unwrap().take() {
            c.stop();
        }
        let live = self.live.lock().unwrap().as_ref().map(Arc::downgrade);
        let get_lang = Box::new(move || live.as_ref().and_then(|l| l.upgrade()).and_then(|l| l.language()));
        let size = Some(self.settings.lock().unwrap().str("translator")).filter(|t| !t.is_empty()).unwrap_or("better".into());
        let app = self.app.clone();
        let emit: Emit = Arc::new(move |kind: &str, data: Value| app.state::<Arc<Backend>>().emit(kind, data));
        let chat = ChatSession::new(channel, &size, get_lang, self.engines.clone(), emit);
        chat.start();
        *self.chat.lock().unwrap() = Some(chat);
    }

    pub(super) fn chat_toggle(&self, on: bool) -> Value {
        if !on {
            if let Some(c) = self.chat.lock().unwrap().take() {
                c.stop();
            }
            self.emit("chat.status", json!({"state": "off"}));
            return json!(true);
        }
        let source = self.live.lock().unwrap().as_ref().filter(|l| !l.is_stopped()).map(|l| l.source());
        if let Some(src) = source {
            if let Ok((Kind::Channel, login)) = classify(&src) {
                self.start_chat(&login);
            }
        }
        json!(true)
    }

    // ---------------------------------------------------------------- caption window + OBS

    pub(super) fn obs_url(&self) -> Result<Value> {
        let mut g = self.obs.lock().unwrap();
        if g.is_none() {
            let server = ObsServer::start(47851)?;
            server.set_style(&self.settings.lock().unwrap().values.get("obs_style").cloned().unwrap_or(json!({})));
            *g = Some(server);
        }
        Ok(json!(g.as_ref().unwrap().url()))
    }

    pub(super) fn overlay_show(&self, show: bool) -> Result<Value> {
        let existing = self.app.get_webview_window("overlay");
        if !show {
            if let Some(w) = existing {
                w.hide()?;
            }
            return Ok(json!(false));
        }
        let w = match existing {
            Some(w) => w,
            None => {
                let scale = (*self.zoom.lock().unwrap()).max(1.0);
                {
                    let st = self.settings.lock().unwrap();
                    let mut o = self.overlay.lock().unwrap();
                    o.scale = scale;
                    o.width = 900.0 * scale;
                    if let Some(v) = st.values.get("overlay_font").and_then(Value::as_f64) {
                        o.font_px = v.clamp(14.0, 80.0);
                    }
                    if let Some(v) = st.values.get("overlay_bg").and_then(Value::as_f64) {
                        o.bg_alpha = v.clamp(0.0, 1.0);
                    }
                }
                let (ww, wh) = (900.0 * scale, 140.0 * scale);
                let w = WebviewWindowBuilder::new(&self.app, "overlay", WebviewUrl::CustomProtocol("overlay://localhost/".parse()?))
                    .title("StreamTranslator captions")
                    .additional_browser_args(&super::browser_args())
                    .decorations(false)
                    .transparent(true)
                    .shadow(false)
                    .always_on_top(true)
                    .skip_taskbar(true)
                    .resizable(false)
                    .focused(false)
                    .visible(false)
                    .inner_size(ww, wh)
                    .build()?;
                // bottom centre of the main screen, a little above the taskbar
                if let Ok(Some(m)) = w.primary_monitor() {
                    let sf = m.scale_factor();
                    let a = m.work_area();
                    let (ax, ay, aw, ah) = (a.position.x as f64 / sf, a.position.y as f64 / sf, a.size.width as f64 / sf, a.size.height as f64 / sf);
                    let _ = w.set_position(tauri::LogicalPosition::new(ax + (aw - ww) / 2.0, ay + ah - wh - 80.0));
                }
                w
            }
        };
        w.show()?;
        let _ = w.set_always_on_top(true);
        self.overlay_push_style();
        Ok(json!(w.is_visible().unwrap_or(true)))
    }

    pub(super) fn overlay_style(&self, p: &Value) -> Value {
        {
            let mut o = self.overlay.lock().unwrap();
            if let Some(v) = p.get("font").and_then(Value::as_f64) {
                o.font_px = v.clamp(14.0, 80.0).trunc();
            }
            if let Some(v) = p.get("bg").and_then(Value::as_f64) {
                o.bg_alpha = v.clamp(0.0, 1.0);
            }
            if let Some(v) = p.get("orig").and_then(Value::as_bool) {
                o.show_orig = v;
            }
        }
        self.overlay_push_style();
        json!(true)
    }

    fn overlay_push_style(&self) {
        let o = self.overlay.lock().unwrap();
        let _ = self.app.emit("overlay-style", json!({"size": (o.font_px * o.scale).trunc(), "bg": o.bg_alpha, "orig": o.show_orig, "hint": (12.0 * o.scale).trunc()}));
    }

    /// From the caption window: "ready", "hide", or "wheel" (text size; with Ctrl, width).
    pub fn overlay_cmd(&self, action: &str, up: bool, ctrl: bool) {
        let Some(w) = self.app.get_webview_window("overlay") else { return };
        match action {
            "hide" => {
                let _ = w.hide();
            }
            "wheel" => {
                let step = if up { 2.0 } else { -2.0 };
                let (width, height) = {
                    let mut o = self.overlay.lock().unwrap();
                    let cur_h = w.inner_size().ok().zip(w.scale_factor().ok()).map(|(s, f)| s.height as f64 / f).unwrap_or(140.0 * o.scale);
                    if ctrl {
                        o.width = (o.width + step * 20.0 * o.scale).max(300.0);
                        (o.width, cur_h)
                    } else {
                        o.font_px = (o.font_px + step).clamp(14.0, 80.0);
                        (o.width, (o.font_px * o.scale * 3.6).max(80.0))
                    }
                };
                let _ = w.set_size(tauri::LogicalSize::new(width, height));
                if !ctrl {
                    self.overlay_push_style();
                }
            }
            _ => self.overlay_push_style(),
        }
    }
}
