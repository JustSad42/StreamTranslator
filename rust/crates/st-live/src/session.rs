//! Live mode (live.py): translate any live stream in near real time.
//!
//! ffmpeg pulls the stream's audio. Voice detection cuts it at natural pauses (or every ~9 s during
//! non-stop talking). Each piece is translated by Whisper and sent out as a caption. While someone is
//! mid-sentence, a fast draft ("partial") is shown and then replaced by the final line.

use std::{
    collections::VecDeque,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use base64::Engine as _;
use serde_json::{json, Value};
use st_core::{
    cancel::Cancel,
    glossary::Glossary,
    langs::{lang_name, nllb_code},
    timefmt::{format_ts, srt_ts},
    JobError,
};
use st_job::{is_junk, Engines};
use st_media::{
    tools::{command, ffmpeg_exe},
    ytdlp::{resolve_live, LiveInfo},
};
use st_ml::{
    isolate,
    vad::{self, VadOptions},
    whisper::{Options, WhisperModel},
};

const SR: usize = 16000;
/// force a cut during non-stop talking
const MAX_CHUNK_S: f64 = 9.0;
/// silence that ends a sentence
const PAUSE_S: f64 = 0.45;
const PARTIAL_EVERY_S: f64 = 1.0;

/// Python's round(x, 1) for the numbers shown in the interface.
fn round1(x: f64) -> f64 {
    format!("{x:.1}").parse().unwrap_or(x)
}
fn round2(x: f64) -> f64 {
    format!("{x:.2}").parse().unwrap_or(x)
}

/// What the user can change while the session runs (live.update).
#[derive(Clone)]
pub struct LiveOptions {
    pub source: String,
    pub model: String,
    pub custom_model: Option<PathBuf>,
    pub language: Option<String>,
    pub show_original: bool,
    pub use_glossary: bool,
    pub champions: Vec<String>,
    pub isolate: bool,
    /// "two-step" or "whisper"
    pub method: String,
    /// NLLB size: "better" or "standard"
    pub translator: String,
    pub save_dir: PathBuf,
}

/// Sends an event to the interface: (type, fields).
pub type Emit = Arc<dyn Fn(&str, Value) + Send + Sync>;

#[derive(Default)]
struct Audio {
    pending: Vec<f32>,
    levels: VecDeque<f64>,
    /// samples received since start
    received: usize,
}

pub struct LiveSession {
    opts: Mutex<LiveOptions>,
    locked_lang: Mutex<Option<String>>,
    /// language -> votes, in the order first heard
    votes: Mutex<Vec<(String, u32)>>,
    pub stop_event: Cancel,
    audio: Mutex<Audio>,
    proc: Mutex<Option<Child>>,
    pub lines: AtomicUsize,
    pub started: Instant,
    pub started_unix: f64,
    pub info: Mutex<LiveInfo>,
    pub transcript_path: Mutex<Option<PathBuf>>,
    pub srt_path: Mutex<Option<PathBuf>>,
    pub thumb_path: Mutex<Option<PathBuf>>,
    last_frame: Mutex<Option<Vec<u8>>>,
    model: Mutex<Option<Arc<WhisperModel>>>,
    engines: Arc<Engines>,
    glossary: Arc<Mutex<Glossary>>,
    emit: Emit,
}

impl LiveSession {
    pub fn new(opts: LiveOptions, engines: Arc<Engines>, glossary: Arc<Mutex<Glossary>>, emit: Emit) -> Arc<LiveSession> {
        Arc::new(LiveSession {
            locked_lang: Mutex::new(opts.language.clone()),
            opts: Mutex::new(opts),
            votes: Mutex::new(vec![]),
            stop_event: Cancel::new(),
            audio: Mutex::new(Audio::default()),
            proc: Mutex::new(None),
            lines: AtomicUsize::new(0),
            started: Instant::now(),
            started_unix: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0),
            info: Mutex::new(LiveInfo::default()),
            transcript_path: Mutex::new(None),
            srt_path: Mutex::new(None),
            thumb_path: Mutex::new(None),
            last_frame: Mutex::new(None),
            model: Mutex::new(None),
            engines,
            glossary,
            emit,
        })
    }

    fn emit(&self, kind: &str, data: Value) {
        (self.emit)(kind, data)
    }
    fn status(&self, state: &str, message: &str) {
        self.emit("live.status", json!({"state": state, "message": message}));
    }

    // ---------------------------------------------------------------- lifecycle

    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        std::thread::spawn(move || me.main());
    }

    pub fn stop(&self) {
        self.stop_event.set();
        if let Some(p) = self.proc.lock().unwrap().as_mut() {
            let _ = p.kill();
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.stop_event.is_set()
    }

    pub fn source(&self) -> String {
        self.opts.lock().unwrap().source.clone()
    }

    /// The language chat translation should use: the detected one once locked, else the chosen one.
    pub fn language(&self) -> Option<String> {
        self.locked_lang.lock().unwrap().clone().or_else(|| self.opts.lock().unwrap().language.clone())
    }

    /// live.update: only the fields the interface sent.
    pub fn update(&self, p: &Value) {
        let mut o = self.opts.lock().unwrap();
        if let Some(v) = p.get("isolate").and_then(Value::as_bool) {
            o.isolate = v;
        }
        if let Some(v) = p.get("method").and_then(Value::as_str) {
            o.method = v.into();
        }
        if let Some(v) = p.get("language") {
            let lang = v.as_str().filter(|l| !l.is_empty()).map(String::from);
            o.language = lang.clone();
            *self.locked_lang.lock().unwrap() = lang;
            self.votes.lock().unwrap().clear();
        }
        if let Some(v) = p.get("orig").and_then(Value::as_bool) {
            o.show_original = v;
        }
        if let Some(v) = p.get("champions").and_then(Value::as_array) {
            o.champions = v.iter().filter_map(|c| c.as_str().map(String::from)).collect();
        }
        if let Some(v) = p.get("use_glossary").and_then(Value::as_bool) {
            o.use_glossary = v;
        }
    }

    fn main(self: Arc<Self>) {
        let r = (|| -> Result<()> {
            self.status("connecting", "Finding the stream…");
            let info = resolve_live(&self.source())?;
            *self.info.lock().unwrap() = info.clone();
            let (model, custom) = {
                let o = self.opts.lock().unwrap();
                (o.model.clone(), o.custom_model.clone())
            };
            self.emit("live.status", json!({"state": "loading", "message": format!("Loading Whisper {model}…"), "channel": info.channel,
                "title": info.title, "thumbnail": info.thumbnail, "url": info.url}));
            let m = self.engines.whisper(&model, custom.as_deref().filter(|_| model == "custom"), &|s| st_appkit::logs::info(s), &|_| {}, &self.stop_event)?;
            *self.model.lock().unwrap() = Some(m);
            self.open_files(&info)?;
            for f in [Self::reader, Self::processor, Self::thumbs, Self::meter] {
                let me = self.clone();
                std::thread::spawn(move || f(&me));
            }
            self.emit("live.status", json!({"state": "live", "message": "Listening…", "device": self.engines.whisper_device(&model),
                "channel": info.channel, "title": info.title, "url": info.url,
                "transcript": self.transcript_path.lock().unwrap().as_ref().map(|p| p.to_string_lossy().into_owned())}));
            while !self.stop_event.wait(1.0) {}
            Ok(())
        })();
        if let Err(e) = r {
            let msg = match e.downcast_ref::<JobError>() {
                Some(j) => j.0.clone(),
                None if e.downcast_ref::<st_core::Cancelled>().is_some() => String::new(),
                None => format!("{e:#}"),
            };
            if !msg.is_empty() {
                self.emit("live.error", json!({"message": msg}));
            }
        }
        self.stop();
        let transcript = self.transcript_path.lock().unwrap().clone();
        if let (Some(frame), Some(t)) = (self.last_frame.lock().unwrap().as_ref(), transcript.as_ref()) {
            // "x.transcript.txt" -> "x.jpg"
            let thumb = t.with_extension("").with_extension("jpg");
            if std::fs::write(&thumb, frame).is_ok() {
                *self.thumb_path.lock().unwrap() = Some(thumb);
            }
        }
        self.emit("live.status", json!({"state": "stopped", "message": "Stopped.", "lines": self.lines.load(Ordering::SeqCst),
            "transcript": transcript.map(|p| p.to_string_lossy().into_owned())}));
    }

    fn open_files(&self, info: &LiveInfo) -> Result<()> {
        let folder = self.opts.lock().unwrap().save_dir.join("Live");
        std::fs::create_dir_all(&folder)?;
        let now = chrono::Local::now();
        let stamp = now.format("%Y-%m-%d_%H-%M");
        let safe: String = info.channel.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(30).collect();
        let transcript = folder.join(format!("{safe}_{stamp}.transcript.txt"));
        let srt = folder.join(format!("{safe}_{stamp}.en.srt"));
        let head = format!("Live translation of {}\n{}\nStarted {}\n\n", info.channel, info.url, now.format("%Y-%m-%d %H:%M"));
        st_core::py::write_text(&transcript, &head)?;
        std::fs::write(&srt, "")?;
        *self.transcript_path.lock().unwrap() = Some(transcript);
        *self.srt_path.lock().unwrap() = Some(srt);
        Ok(())
    }

    // ---------------------------------------------------------------- audio in

    fn reader(&self) {
        let mut failures = 0u32;
        while !self.stop_event.is_set() {
            let info = self.info.lock().unwrap().clone();
            let Ok(ff) = ffmpeg_exe() else {
                self.emit("live.error", json!({"message": "ffmpeg was not found."}));
                self.stop();
                return;
            };
            let mut cmd = command(&ff);
            cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error"]);
            if !info.ua.is_empty() {
                cmd.args(["-user_agent", &info.ua]);
            }
            cmd.args(["-i", &info.audio, "-vn", "-ac", "1", "-ar", &SR.to_string(), "-f", "f32le", "-"]);
            let mut got_any = false;
            if let Ok(mut child) = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
                let mut out = child.stdout.take().unwrap();
                // keep ffmpeg's last complaint, for the log when the stream drops
                let mut err = child.stderr.take().unwrap();
                let last_err = std::thread::spawn(move || {
                    let mut e = String::new();
                    let _ = err.read_to_string(&mut e);
                    e.lines().last().unwrap_or("").to_string()
                });
                *self.proc.lock().unwrap() = Some(child);
                if self.stop_event.is_set() {
                    self.stop();
                }
                let mut buf = vec![0u8; SR / 10 * 4]; // 0.1 s
                while !self.stop_event.is_set() {
                    let n = read_full(&mut out, &mut buf);
                    if n < 4 {
                        break;
                    }
                    got_any = true;
                    let a: Vec<f32> = buf[..n - n % 4].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
                    let rms = (a.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / a.len() as f64).sqrt();
                    let db = 20.0 * rms.max(1e-6).log10();
                    let mut au = self.audio.lock().unwrap();
                    au.received += a.len();
                    au.pending.extend(a);
                    au.levels.push_back((((db + 60.0) / 50.0).clamp(0.0, 1.0) * 1000.0).round() / 1000.0);
                }
                if let Some(mut c) = self.proc.lock().unwrap().take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                let why = last_err.join().unwrap_or_default();
                if !self.stop_event.is_set() {
                    st_appkit::logs::warning(&format!("Live stream audio stopped{}", if why.is_empty() { String::new() } else { format!(": {why}") }));
                }
            }
            if self.stop_event.is_set() {
                break;
            }
            failures = if got_any { 0 } else { failures + 1 };
            if failures >= 5 {
                self.emit("live.error", json!({"message": "Lost the stream and couldn't reconnect. It may have ended."}));
                self.stop();
                break;
            }
            self.status("reconnecting", "Stream dropped. Reconnecting…");
            if self.stop_event.wait((2 * failures + 1).min(8) as f64) {
                break;
            }
            match resolve_live(&self.source()) {
                Ok(fresh) => {
                    let mut i = self.info.lock().unwrap();
                    i.audio = fresh.audio;
                    i.preview = fresh.preview;
                    drop(i);
                    self.status("live", "Listening…");
                }
                Err(e) if e.to_string().contains("isn't live") => {
                    self.emit("live.error", json!({"message": "The stream ended."}));
                    self.stop();
                }
                Err(_) => {}
            }
        }
    }

    /// Twitch delivers audio in ~2 s bursts. Replay the levels at real-time pace so the meter moves
    /// smoothly instead of jumping once per burst.
    fn meter(&self) {
        while !self.stop_event.wait(0.1) {
            let lv = {
                let mut au = self.audio.lock().unwrap();
                if au.levels.len() > 40 {
                    let drop = au.levels.len() - 20;
                    au.levels.drain(..drop);
                }
                au.levels.pop_front()
            };
            if let Some(lv) = lv {
                self.emit("live.level", json!({"level": lv}));
            }
        }
    }

    fn thumbs(&self) {
        let mut wait = 1.0;
        while !self.stop_event.wait(wait) {
            wait = 20.0;
            let Some(url) = self.info.lock().unwrap().preview.clone() else { return };
            let Ok(ff) = ffmpeg_exe() else { return };
            let args = ["-hide_banner", "-nostdin", "-loglevel", "error", "-i", &url, "-frames:v", "1", "-vf", "scale=640:-2", "-f", "image2pipe", "-vcodec", "mjpeg", "-"];
            if let Some(jpg) = run_with_timeout(command(&ff).args(args), Duration::from_secs(25)) {
                let data = format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&jpg));
                *self.last_frame.lock().unwrap() = Some(jpg);
                self.emit("live.frame", json!({"data": data}));
            }
        }
    }

    // ---------------------------------------------------------------- speech out

    fn processor(&self) {
        let vad_opts = VadOptions { threshold: 0.5, min_silence_ms: 350.0, speech_pad_ms: 150.0, min_speech_ms: 200.0 };
        let mut buf: Vec<f32> = vec![];
        let mut buf_start = 0usize; // absolute sample index of buf[0]
        let mut last_partial = Instant::now() - Duration::from_secs(3600);
        let pad = (0.15 * SR as f64) as usize;
        while !self.stop_event.wait(0.25) {
            {
                let mut au = self.audio.lock().unwrap();
                buf.append(&mut au.pending);
            }
            if (buf.len() as f64) < 0.8 * SR as f64 {
                continue;
            }
            let mut speech = match vad::speech_timestamps(&buf, &vad_opts) {
                Ok(s) => s,
                Err(e) => {
                    self.emit("live.error", json!({"message": format!("Voice detection failed: {e:#}")}));
                    self.stop();
                    return;
                }
            };
            if speech.is_empty() {
                let keep = (0.4 * SR as f64) as usize;
                let cut = buf.len().saturating_sub(keep);
                buf_start += cut;
                buf.drain(..cut);
                continue;
            }
            let first = speech[0].0.saturating_sub(pad);
            if first > 0 {
                buf.drain(..first);
                buf_start += first;
                speech = speech.iter().map(|&(a, b)| (a - first, b - first)).collect();
            }
            let last_end = speech[speech.len() - 1].1;
            let trailing = buf.len() - last_end;
            let pause = PAUSE_S * SR as f64;
            if trailing as f64 >= pause || buf.len() as f64 >= MAX_CHUNK_S * SR as f64 {
                let cut = if trailing as f64 >= pause {
                    buf.len().min(last_end + pad)
                } else {
                    // the longest pause after the first 3 s (later end wins a tie, like Python's max on tuples)
                    let gaps: Vec<(usize, usize)> = (0..speech.len() - 1).filter(|&i| speech[i].1 > 3 * SR).map(|i| (speech[i + 1].0 - speech[i].1, speech[i].1)).collect();
                    match gaps.iter().max() {
                        Some(&(g, end)) if g as f64 > 0.12 * SR as f64 => end + pad / 2,
                        _ => buf.len(),
                    }
                };
                let cut = cut.min(buf.len());
                let chunk: Vec<f32> = buf[..cut].to_vec();
                if let Err(e) = self.final_line(&chunk, buf_start, buf_start + cut) {
                    if !self.stop_event.is_set() {
                        self.emit("live.error", json!({"message": format!("Translation error: {e:#}"), "fatal": false}));
                    }
                }
                buf.drain(..cut);
                buf_start += cut;
                last_partial = Instant::now();
            } else if last_partial.elapsed().as_secs_f64() >= PARTIAL_EVERY_S && (last_end - speech[0].0) as f64 > 1.2 * SR as f64 {
                last_partial = Instant::now();
                let _ = self.partial(&buf);
            }
        }
    }

    fn model(&self) -> Arc<WhisperModel> {
        self.model.lock().unwrap().clone().expect("model loaded")
    }

    fn fix(&self, text: &str, english: bool) -> String {
        if !self.opts.lock().unwrap().use_glossary {
            return text.to_string();
        }
        self.glossary.lock().unwrap().fix(text, english, &[]).0
    }

    fn detect(&self, chunk: &[f32]) -> Result<String> {
        if let Some(l) = self.locked_lang.lock().unwrap().clone() {
            return Ok(l);
        }
        let (lang, prob, _) = self.model().detect_language(chunk, 1, 0.5)?;
        let mut votes = self.votes.lock().unwrap();
        if prob >= 0.5 {
            let add = if prob >= 0.9 { 2 } else { 1 };
            match votes.iter_mut().find(|(l, _)| *l == lang) {
                Some(v) => v.1 += add,
                None => votes.push((lang.clone(), add)),
            }
        }
        // the first language with the most votes, like Python's max()
        let mut best = (lang.clone(), 0);
        for (l, c) in votes.iter() {
            if *c > best.1 {
                best = (l.clone(), *c);
            }
        }
        if best.1 >= 3 {
            *self.locked_lang.lock().unwrap() = Some(best.0.clone());
            self.emit("live.lang", json!({"lang": best.0, "name": lang_name(Some(&best.0)), "locked": true}));
        } else {
            self.emit("live.lang", json!({"lang": lang, "name": lang_name(Some(&lang)), "locked": false, "prob": round2(prob as f64)}));
        }
        Ok(lang)
    }

    /// Optional voice isolation (game sound / music removed) before Whisper.
    fn clean(&self, chunk: &[f32]) -> Vec<f32> {
        if !self.opts.lock().unwrap().isolate {
            return chunk.to_vec();
        }
        let r = (|| -> Result<Vec<f32>> {
            if !isolate::model_ready() {
                self.status("live", "Downloading the voice isolation model (66 MB)…");
                isolate::download_model(&|_| {})?;
                self.status("live", "Listening…");
            }
            isolate::voice_16k_from_16k(chunk, &self.stop_event)
        })();
        match r {
            Ok(v) => v,
            Err(e) => {
                self.opts.lock().unwrap().isolate = false;
                self.emit("live.error", json!({"message": format!("Voice isolation turned off: {e:#}"), "fatal": false}));
                chunk.to_vec()
            }
        }
    }

    fn transcribe(&self, chunk: &[f32], lang: &str, task: &str, beam: usize, hot: Option<&str>) -> Result<String> {
        let mut o = Options::new(lang, task, hot);
        o.beam_size = beam;
        o.vad = false;
        o.without_timestamps = true;
        let segs = self.model().transcribe(chunk, &o, &|_| {}, &mut |_| {}, &self.stop_event)?;
        let gl = self.glossary.lock().unwrap();
        let parts: Vec<String> = segs
            .iter()
            .filter_map(|s| {
                let t = s.text.split_whitespace().collect::<Vec<_>>().join(" ");
                (!t.is_empty() && !is_junk(s, &t) && !gl.is_prompt_echo(&t, hot)).then_some(t)
            })
            .collect();
        Ok(parts.join(" "))
    }

    fn two_step(&self, lang: &str) -> bool {
        self.opts.lock().unwrap().method == "two-step" && lang != "en" && nllb_code(lang).is_some()
    }

    fn to_english(&self, text: &str, lang: &str, beam: usize) -> Result<String> {
        let (size, use_glossary) = {
            let o = self.opts.lock().unwrap();
            (o.translator.clone(), o.use_glossary)
        };
        let tr = self.engines.translator(&size, &|s| self.status("live", s), &|_| {}, &Cancel::new())?;
        let text = if use_glossary { self.glossary.lock().unwrap().prepare(text, Some(lang)).0 } else { text.to_string() };
        Ok(tr.translate(&[text], lang, beam)?.remove(0))
    }

    /// (english, original) for a piece, with either method.
    fn speech(&self, chunk: &[f32], lang: &str, beam: usize) -> Result<(String, String)> {
        let (use_glossary, champions, show_original) = {
            let o = self.opts.lock().unwrap();
            (o.use_glossary, o.champions.clone(), o.show_original)
        };
        if self.two_step(lang) {
            let hot = if use_glossary { self.glossary.lock().unwrap().hotwords(&champions, false, &[]) } else { None };
            let orig = self.transcribe(chunk, lang, "transcribe", beam, hot.as_deref())?;
            if orig.is_empty() {
                return Ok((String::new(), String::new()));
            }
            let en = self.to_english(&orig, lang, if beam == 1 { 2 } else { 4 })?;
            return Ok((self.fix(&en, true), self.fix(&orig, false)));
        }
        let task = if lang == "en" { "transcribe" } else { "translate" };
        let hot = if use_glossary { self.glossary.lock().unwrap().hotwords(&champions, true, &[]) } else { None };
        let text = self.transcribe(chunk, lang, task, beam, hot.as_deref())?;
        if text.is_empty() {
            return Ok((String::new(), String::new()));
        }
        let mut orig = String::new();
        if beam > 1 && show_original && lang != "en" {
            orig = self.fix(&self.transcribe(chunk, lang, "transcribe", 3, None)?, false);
        }
        Ok((self.fix(&text, true), orig))
    }

    fn partial(&self, buf: &[f32]) -> Result<()> {
        let Some(lang) = self.locked_lang.lock().unwrap().clone() else { return Ok(()) };
        let (text, _) = self.speech(&self.clean(buf), &lang, 1)?;
        if !text.is_empty() && !self.stop_event.is_set() {
            self.emit("live.partial", json!({"text": text}));
        }
        Ok(())
    }

    fn final_line(&self, chunk: &[f32], start_sample: usize, end_sample: usize) -> Result<()> {
        let t_proc = Instant::now();
        let chunk = self.clean(chunk);
        let lang = self.detect(&chunk)?;
        let (text, mut orig) = self.speech(&chunk, &lang, 5)?;
        if text.is_empty() {
            self.emit("live.partial", json!({"text": ""}));
            return Ok(());
        }
        if !self.opts.lock().unwrap().show_original {
            orig.clear();
        }
        let received = self.audio.lock().unwrap().received;
        let delay = (received as f64 - end_sample as f64) / SR as f64;
        let (t0, t1) = (start_sample as f64 / SR as f64, end_sample as f64 / SR as f64);
        let n = self.lines.fetch_add(1, Ordering::SeqCst) + 1;
        let now = chrono::Local::now();
        self.emit("live.caption", json!({"id": n, "text": text, "orig": orig, "lang": lang, "t": round1(t0),
            "clock": now.format("%H:%M:%S").to_string(), "delay": round1(delay), "took": round2(t_proc.elapsed().as_secs_f64())}));
        let mut tx = format!("[{} | +{}]  {}\n", now.format("%H:%M:%S"), format_ts(t0), text);
        if !orig.is_empty() {
            tx += &format!("{:>22}({})\n", "", orig);
        }
        let srt = format!("{n}\n{} --> {}\n{}\n\n", srt_ts(t0), srt_ts(t1.max(t0 + 1.2)), text);
        let (tp, sp) = (self.transcript_path.lock().unwrap().clone(), self.srt_path.lock().unwrap().clone());
        if let (Some(tp), Some(sp)) = (tp, sp) {
            let _ = append(&tp, &tx).and_then(|_| append(&sp, &srt));
        }
        Ok(())
    }
}

/// Append in Python's text mode (Windows line endings).
fn append(path: &Path, text: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new().append(true).create(true).open(path)?;
    f.write_all(text.replace('\n', "\r\n").as_bytes())
}

/// Read until the buffer is full or the stream ends (Python's BufferedReader.read(n)).
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> usize {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    n
}

/// A command's stdout, or None if it failed or ran longer than `limit`.
fn run_with_timeout(cmd: &mut std::process::Command, limit: Duration) -> Option<Vec<u8>> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut v = vec![];
        let _ = out.read_to_end(&mut v);
        v
    });
    let t0 = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                let data = reader.join().ok()?;
                return (st.success() && !data.is_empty()).then_some(data);
            }
            Ok(None) if t0.elapsed() < limit => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}
