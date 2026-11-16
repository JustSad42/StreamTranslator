//! Team voice: live English captions of your team's voice call, with each line labelled by who said it.
//!
//! Two sources, each listened to separately: the device the team call plays on (a headset, speakers or a
//! virtual cable; recorded in loopback) and your own microphone. Voice detection cuts each source into
//! sentences at natural pauses. Sentences from the team device are matched against voices learned
//! before the game (a ~10 s fingerprint per teammate); sentences from your mic are yours. Whisper then
//! translates them straight to English, which keeps the English words and League calls that Egyptian
//! (and other) players mix in, and handles dialect better than transcribing first (docs/TEAM_VOICE_TESTS.md).

use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use st_core::{cancel::Cancel, glossary::Glossary, langs::lang_name, timefmt::format_ts, JobError};
use st_job::{is_junk, Engines};
use st_ml::{
    speakers,
    vad::{self, VadOptions},
    whisper::{Options, WhisperModel},
};

use crate::{devices, discord, Emit};

const SR: usize = 16000;
/// silence that ends a sentence
const PAUSE_S: f64 = 0.5;
/// force a cut during non-stop talking (short enough to read on stream)
const MAX_CHUNK_S: f64 = 6.0;
/// speech needed to learn a voice
pub const LEARN_SPEECH_S: f64 = 8.0;
/// give up learning after this long
const LEARN_TIMEOUT_S: f64 = 45.0;
/// fingerprint similarity needed to put a name on a sentence
pub const MATCH: f32 = 0.42;
/// a sentence this short is too little to recognise the voice; it keeps the previous speaker
const MIN_ID_S: f64 = 0.8;
/// shorter than this isn't words
const MIN_WORDS_S: f64 = 0.35;
pub const UNKNOWN: &str = "Teammate";
const UNKNOWN_COLOR: &str = "#b9b9c8";
const ME_COLOR: &str = "#5eead4";

/// Colours handed out to learned voices, in order.
pub const PALETTE: &[&str] = &["#ffd166", "#ff7aa8", "#7cc4ff", "#b794ff", "#8ef0a0", "#ffa36c", "#f4f1a6", "#ff8a8a"];

/// A teammate's learned voice (saved in team_voices.json).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Voice {
    pub name: String,
    pub color: String,
    pub emb: Vec<f32>,
}

fn voices_path() -> PathBuf {
    st_core::paths::data_dir().join("team_voices.json")
}

pub fn load_voices() -> Vec<Voice> {
    std::fs::read(voices_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_voices(v: &[Voice]) -> Result<()> {
    let p = voices_path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(v)?)?;
    std::fs::rename(tmp, p)?;
    Ok(())
}

/// The voices without their fingerprints, for the interface.
pub fn voices_view(v: &[Voice]) -> Value {
    json!(v.iter().map(|x| json!({"name": x.name, "color": x.color})).collect::<Vec<_>>())
}

/// The learned voice that best matches `emb`, if it's close enough.
pub fn identify<'a>(voices: &'a [Voice], emb: &[f32]) -> Option<(&'a Voice, f32)> {
    voices.iter().map(|v| (v, speakers::similarity(&v.emb, emb))).filter(|(_, s)| *s >= MATCH).max_by(|a, b| a.1.total_cmp(&b.1))
}

/// Someone heard through the Discord bot (saved in team_discord.json, so colours and renames stick).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Person {
    pub id: String,
    /// their Discord display name, as last seen
    pub discord: String,
    /// your own name for them, if you renamed them
    #[serde(default)]
    pub name: Option<String>,
    pub color: String,
}

impl Person {
    pub fn shown(&self) -> &str {
        self.name.as_deref().filter(|n| !n.is_empty()).unwrap_or(&self.discord)
    }
}

fn people_path() -> PathBuf {
    st_core::paths::data_dir().join("team_discord.json")
}

pub fn load_people() -> Vec<Person> {
    std::fs::read(people_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_people(v: &[Person]) -> Result<()> {
    let p = people_path();
    std::fs::create_dir_all(p.parent().unwrap())?;
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(v)?)?;
    std::fs::rename(tmp, p)?;
    Ok(())
}

pub fn person_view(p: &Person) -> Value {
    json!({"id": p.id, "name": p.shown(), "discord": p.discord, "renamed": p.name.is_some(), "color": p.color})
}

// ---------------------------------------------------------------- sources

/// Sound from a device, or (for tests) a file played in real time: "file:C:\x.wav".
pub enum Source {
    Device(devices::Capture),
    File(Cancel),
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Source::File(c) = self {
            c.set();
        }
    }
}

/// Start a source; its 16 kHz mono samples are appended to `sink`.
pub fn open_source(id: &str, sink: Arc<Mutex<Vec<f32>>>, on_error: impl Fn(String) + Send + 'static) -> Result<Source> {
    if let Some(path) = id.strip_prefix("file:") {
        let audio = st_media::ffmpeg::decode_audio(Path::new(path))?;
        let stop = Cancel::new();
        let c = stop.clone();
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let mut sent = 0usize;
            while sent < audio.len() && !c.is_set() {
                let due = ((t0.elapsed().as_secs_f64() * SR as f64) as usize).min(audio.len());
                if due > sent {
                    sink.lock().unwrap().extend_from_slice(&audio[sent..due]);
                    sent = due;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // then silence, like a quiet call
            while !c.wait(0.1) {
                sink.lock().unwrap().extend(std::iter::repeat_n(0.0, SR / 10));
            }
        });
        return Ok(Source::File(stop));
    }
    let cap = devices::capture(id, move |a| sink.lock().unwrap().extend_from_slice(a), on_error)?;
    Ok(Source::Device(cap))
}

/// Cuts a source's sound into sentences at pauses (like the live session's processor).
struct Segmenter {
    buf: Vec<f32>,
    /// absolute sample index of buf[0]
    start: usize,
    vad: VadOptions,
}

impl Segmenter {
    fn new() -> Segmenter {
        Segmenter { buf: vec![], start: 0, vad: VadOptions { threshold: 0.5, min_silence_ms: 350.0, speech_pad_ms: 150.0, min_speech_ms: 250.0 } }
    }

    /// Add sound; returns finished sentences as (audio, start sample, end sample).
    fn push(&mut self, more: &mut Vec<f32>) -> Result<Vec<(Vec<f32>, usize, usize)>> {
        self.buf.append(more);
        let mut out = vec![];
        if (self.buf.len() as f64) < 0.6 * SR as f64 {
            return Ok(out);
        }
        let pad = (0.15 * SR as f64) as usize;
        let mut speech = vad::speech_timestamps(&self.buf, &self.vad)?;
        if speech.is_empty() {
            let keep = (0.4 * SR as f64) as usize;
            let cut = self.buf.len().saturating_sub(keep);
            self.start += cut;
            self.buf.drain(..cut);
            return Ok(out);
        }
        let first = speech[0].0.saturating_sub(pad);
        if first > 0 {
            self.buf.drain(..first);
            self.start += first;
            speech = speech.iter().map(|&(a, b)| (a - first, b - first)).collect();
        }
        let last_end = speech[speech.len() - 1].1;
        let trailing = self.buf.len() - last_end;
        if trailing as f64 >= PAUSE_S * SR as f64 || self.buf.len() as f64 >= MAX_CHUNK_S * SR as f64 {
            let cut = if trailing as f64 >= PAUSE_S * SR as f64 {
                self.buf.len().min(last_end + pad)
            } else {
                let gaps: Vec<(usize, usize)> = (0..speech.len() - 1).filter(|&i| speech[i].1 > 3 * SR).map(|i| (speech[i + 1].0 - speech[i].1, speech[i].1)).collect();
                match gaps.iter().max() {
                    Some(&(g, end)) if g as f64 > 0.12 * SR as f64 => end + pad / 2,
                    _ => self.buf.len(),
                }
            }
            .min(self.buf.len());
            out.push((self.buf[..cut].to_vec(), self.start, self.start + cut));
            self.buf.drain(..cut);
            self.start += cut;
        }
        Ok(out)
    }
}

fn level(a: &[f32]) -> f64 {
    if a.is_empty() {
        return 0.0;
    }
    let rms = (a.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / a.len() as f64).sqrt();
    (((20.0 * rms.max(1e-6).log10() + 60.0) / 50.0).clamp(0.0, 1.0) * 1000.0).round() / 1000.0
}

// ---------------------------------------------------------------- learning a voice

/// Listen to `device` until LEARN_SPEECH_S of speech is heard (only the one teammate should talk) and
/// return their fingerprint. `progress(fraction of speech heard, level)`.
pub fn learn_voice(device: &str, progress: &dyn Fn(f64, f64), cancel: &Cancel) -> Result<Vec<f32>> {
    if !speakers::model_ready() {
        speakers::download_model(&|_| {})?;
    }
    let sink = Arc::new(Mutex::new(vec![]));
    let errors = Arc::new(Mutex::new(None::<String>));
    let e2 = errors.clone();
    let _src = open_source(device, sink.clone(), move |e| *e2.lock().unwrap() = Some(e))?;
    let mut seg = Segmenter::new();
    let mut speech: Vec<f32> = vec![];
    let t0 = Instant::now();
    while (speech.len() as f64) < LEARN_SPEECH_S * SR as f64 {
        if cancel.wait(0.2) {
            return Err(st_core::Cancelled.into());
        }
        if let Some(e) = errors.lock().unwrap().take() {
            return Err(JobError(format!("The audio device stopped: {e}")).into());
        }
        if t0.elapsed().as_secs_f64() > LEARN_TIMEOUT_S {
            return Err(JobError(if speech.is_empty() {
                "Didn't hear anyone on that device. Check that the call plays on it, then try again.".into()
            } else {
                format!("Only heard {:.0} s of talking. Ask them to keep talking for about 10 seconds, then try again.", speech.len() as f64 / SR as f64)
            })
            .into());
        }
        let mut more = std::mem::take(&mut *sink.lock().unwrap());
        let lv = level(&more);
        for (a, s, e) in seg.push(&mut more)? {
            // only the voiced parts, so pauses don't water the fingerprint down
            let parts = vad::speech_timestamps(&a, &seg.vad)?;
            for (x, y) in parts {
                speech.extend_from_slice(&a[x..y]);
            }
            let _ = (s, e);
        }
        progress((speech.len() as f64 / (LEARN_SPEECH_S * SR as f64)).min(1.0), lv);
    }
    // average a few 3 s fingerprints: steadier than one long one
    let win = 3 * SR;
    let mut acc = vec![0f32; 0];
    let mut n = 0;
    for chunk in speech.chunks(win).filter(|c| c.len() >= SR * 3 / 2) {
        let e = speakers::embedding(chunk)?;
        if acc.is_empty() {
            acc = vec![0.0; e.len()];
        }
        acc.iter_mut().zip(&e).for_each(|(a, b)| *a += b);
        n += 1;
    }
    if n == 0 {
        return Err(JobError("Not enough clear speech to learn the voice.".into()).into());
    }
    let norm = acc.iter().map(|v| v * v).sum::<f32>().sqrt() + 1e-9;
    Ok(acc.iter().map(|v| v / norm).collect())
}

// ---------------------------------------------------------------- the session

#[derive(Clone)]
pub struct TeamOptions {
    /// where the team call plays (recorded in loopback if it's a playback device)
    pub team_device: Option<String>,
    /// your microphone, captioned under `my_name`
    pub mic_device: Option<String>,
    pub my_name: String,
    pub model: String,
    pub custom_model: Option<PathBuf>,
    /// None = detect per sentence
    pub language: Option<String>,
    pub use_glossary: bool,
    pub champions: Vec<String>,
    pub save_dir: PathBuf,
    pub voices: Vec<Voice>,
    /// hear the call through a Discord bot instead of the devices (each person separately, by name)
    pub discord: Option<discord::DiscordTarget>,
}

/// One sentence waiting to be translated.
struct Sentence {
    /// 0 = team device, 1 = your mic
    source: usize,
    /// the Discord user who said it (Discord mode)
    user: Option<u64>,
    audio: Vec<f32>,
    t0: f64,
    heard: Instant,
}

pub struct TeamSession {
    opts: Mutex<TeamOptions>,
    pub stop_event: Cancel,
    pub lines: AtomicUsize,
    pub started: Instant,
    pub transcript_path: Mutex<Option<PathBuf>>,
    model: Mutex<Option<Arc<WhisperModel>>>,
    engines: Arc<Engines>,
    glossary: Arc<Mutex<Glossary>>,
    emit: Emit,
    /// the last named speaker on the team device and when, for sentences too short to recognise
    last_speaker: Mutex<Option<(String, String, Instant)>>,
    /// Discord mode: everyone seen so far, and who is in this call
    people: Mutex<Vec<Person>>,
    in_call: Mutex<Vec<u64>>,
}

impl TeamSession {
    pub fn new(opts: TeamOptions, engines: Arc<Engines>, glossary: Arc<Mutex<Glossary>>, emit: Emit) -> Arc<TeamSession> {
        Arc::new(TeamSession {
            opts: Mutex::new(opts),
            stop_event: Cancel::new(),
            lines: AtomicUsize::new(0),
            started: Instant::now(),
            transcript_path: Mutex::new(None),
            model: Mutex::new(None),
            engines,
            glossary,
            emit,
            last_speaker: Mutex::new(None),
            people: Mutex::new(load_people()),
            in_call: Mutex::new(vec![]),
        })
    }

    fn emit(&self, kind: &str, data: Value) {
        (self.emit)(kind, data)
    }

    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        std::thread::spawn(move || me.main());
    }

    pub fn stop(&self) {
        self.stop_event.set();
    }

    pub fn is_stopped(&self) -> bool {
        self.stop_event.is_set()
    }

    /// team.update: settings that can change mid-session.
    pub fn update(&self, p: &Value) {
        let mut o = self.opts.lock().unwrap();
        if let Some(v) = p.get("language") {
            o.language = v.as_str().filter(|l| !l.is_empty()).map(String::from);
        }
        if let Some(v) = p.get("use_glossary").and_then(Value::as_bool) {
            o.use_glossary = v;
        }
        if let Some(v) = p.get("champions").and_then(Value::as_array) {
            o.champions = v.iter().filter_map(|c| c.as_str().map(String::from)).collect();
        }
        if let Some(v) = p.get("my_name").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
            o.my_name = v.trim().into();
        }
    }

    /// Voices learned or removed while running.
    pub fn set_voices(&self, v: Vec<Voice>) {
        self.opts.lock().unwrap().voices = v;
    }

    /// Discord people renamed while running.
    pub fn set_people(&self, v: Vec<Person>) {
        *self.people.lock().unwrap() = v;
        self.emit_people();
    }

    /// team.people: who the bot has heard in this call.
    fn emit_people(&self) {
        let ids = self.in_call.lock().unwrap().clone();
        let people = self.people.lock().unwrap();
        let list: Vec<Value> = ids.iter().filter_map(|id| people.iter().find(|p| p.id == id.to_string())).map(person_view).collect();
        self.emit("team.people", json!({"people": list}));
    }

    /// A Discord user's display name arrived: remember them (keeping any rename and colour).
    fn met(&self, user: u64, discord_name: &str) {
        {
            let mut people = self.people.lock().unwrap();
            let id = user.to_string();
            match people.iter_mut().find(|p| p.id == id) {
                Some(p) => p.discord = discord_name.to_string(),
                None => {
                    let color = PALETTE[people.len() % PALETTE.len()].to_string();
                    people.push(Person { id, discord: discord_name.to_string(), name: None, color });
                }
            }
            let _ = save_people(&people);
        }
        {
            let mut ids = self.in_call.lock().unwrap();
            if !ids.contains(&user) {
                ids.push(user);
            }
        }
        self.emit_people();
    }

    fn person(&self, user: u64) -> (String, String) {
        let my = self.opts.lock().unwrap().my_name.clone();
        let people = self.people.lock().unwrap();
        match people.iter().find(|p| p.id == user.to_string()) {
            Some(p) if !my.is_empty() && p.shown().eq_ignore_ascii_case(&my) => (p.shown().to_string(), ME_COLOR.into()),
            Some(p) => (p.shown().to_string(), p.color.clone()),
            None => (UNKNOWN.into(), UNKNOWN_COLOR.into()),
        }
    }

    /// Discord mode: the bot joins the call; each person gets their own sentence cutter.
    fn start_discord(self: &Arc<Self>, target: discord::DiscordTarget, tx: mpsc::Sender<Sentence>) -> Result<discord::DiscordCall> {
        let sinks: Arc<Mutex<HashMap<u64, Arc<Mutex<Vec<f32>>>>>> = Default::default();
        let tx = Mutex::new(tx);
        let (me1, me2, me3, me4) = (self.clone(), self.clone(), self.clone(), self.clone());
        discord::DiscordCall::start(
            target,
            discord::Callbacks {
                audio: Box::new(move |user, pcm| {
                    let mut map = sinks.lock().unwrap();
                    let sink = map.entry(user).or_insert_with(|| {
                        let sink = Arc::new(Mutex::new(vec![]));
                        let (me, s, tx) = (me1.clone(), sink.clone(), tx.lock().unwrap().clone());
                        std::thread::spawn(move || me.listen(0, Some(user), s, tx));
                        sink
                    });
                    sink.lock().unwrap().extend_from_slice(pcm);
                }),
                name: Box::new(move |user, name| me2.met(user, name)),
                problem: Box::new(move |msg, fatal| {
                    me3.emit("team.error", json!({"message": msg, "fatal": fatal}));
                    if fatal {
                        me3.stop();
                    }
                }),
                joined: Box::new(move |channel| {
                    me4.emit("team.status", json!({"state": "live", "message": format!("Listening in {channel}…"),
                        "transcript": me4.transcript_path.lock().unwrap().as_ref().map(|p| p.to_string_lossy().into_owned())}));
                }),
            },
        )
    }

    fn main(self: Arc<Self>) {
        let r = (|| -> Result<()> {
            let o = self.opts.lock().unwrap().clone();
            if o.discord.is_none() && o.team_device.is_none() && o.mic_device.is_none() {
                return Err(JobError("Pick the device your team call plays on, your microphone, or both.".into()).into());
            }
            self.emit("team.status", json!({"state": "loading", "message": format!("Loading Whisper {}…", o.model)}));
            let m = self.engines.whisper(&o.model, o.custom_model.as_deref().filter(|_| o.model == "custom"), &|s| st_appkit::logs::info(s), &|_| {}, &self.stop_event)?;
            *self.model.lock().unwrap() = Some(m);
            if o.discord.is_none() && o.team_device.is_some() && !o.voices.is_empty() && !speakers::model_ready() {
                self.emit("team.status", json!({"state": "loading", "message": "Downloading the voice model (26 MB)…"}));
                speakers::download_model(&|_| {})?;
            }
            self.open_transcript()?;
            // warm up the GPU and the voice model, so the first line isn't slower than the rest
            self.emit("team.status", json!({"state": "loading", "message": "Getting ready…"}));
            let quiet = vec![0.0f32; SR];
            if let Some(m) = self.model.lock().unwrap().clone() {
                let mut wo = Options::new("en", "transcribe", None);
                wo.vad = false;
                wo.without_timestamps = true;
                let _ = m.transcribe(&quiet, &wo, &|_| {}, &mut |_| {}, &self.stop_event);
            }
            if o.discord.is_none() && o.team_device.is_some() && !o.voices.is_empty() {
                let _ = speakers::embedding(&quiet);
            }
            if let Some(target) = o.discord.clone() {
                let (tx, rx) = mpsc::channel::<Sentence>();
                let me = self.clone();
                std::thread::spawn(move || me.worker(rx));
                self.emit("team.status", json!({"state": "loading", "message": "Joining the Discord call…"}));
                // test hook: each WAV in the folder plays as one person in the call, named after the file
                if let Ok(dir) = std::env::var("ST_DISCORD_FAKE") {
                    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "wav")).collect();
                    files.sort();
                    for (i, f) in files.into_iter().enumerate() {
                        let user = 1000 + i as u64;
                        let audio = st_media::ffmpeg::decode_audio(&f)?;
                        self.met(user, &f.file_stem().unwrap().to_string_lossy());
                        let sink = Arc::new(Mutex::new(vec![]));
                        let (me, s, tx) = (self.clone(), sink.clone(), tx.clone());
                        std::thread::spawn(move || me.listen(0, Some(user), s, tx));
                        let me = self.clone();
                        std::thread::spawn(move || {
                            let t0 = Instant::now();
                            let mut sent = 0;
                            while !me.stop_event.wait(0.02) {
                                let due = ((t0.elapsed().as_secs_f64() * SR as f64) as usize).min(audio.len());
                                let mut s = sink.lock().unwrap();
                                s.extend_from_slice(&audio[sent..due]);
                                if due == audio.len() {
                                    s.extend(std::iter::repeat_n(0.0, SR / 50));
                                }
                                sent = due;
                            }
                        });
                    }
                    self.emit("team.status", json!({"state": "live", "message": "Listening in test call…",
                        "transcript": self.transcript_path.lock().unwrap().as_ref().map(|p| p.to_string_lossy().into_owned())}));
                    while !self.stop_event.wait(0.5) {}
                    return Ok(());
                }
                let call = self.start_discord(target, tx)?;
                while !self.stop_event.wait(0.5) {}
                drop(call);
                return Ok(());
            }

            // capture both sources here: audio streams stay on the thread that opened them
            let (tx, rx) = mpsc::channel::<Sentence>();
            let mut sources = vec![];
            for (i, dev) in [&o.team_device, &o.mic_device].into_iter().enumerate() {
                let Some(dev) = dev else { continue };
                let sink = Arc::new(Mutex::new(vec![]));
                let me = self.clone();
                let label = if i == 0 { "team call" } else { "microphone" };
                let src = open_source(dev, sink.clone(), move |e| {
                    me.emit("team.error", json!({"message": format!("The {label} device reported a problem: {e}"), "fatal": false}))
                })
                .map_err(|e| JobError(format!("Couldn't open the {label} device: {e:#}")))?;
                sources.push(src);
                let (me, tx) = (self.clone(), tx.clone());
                std::thread::spawn(move || me.listen(i, None, sink, tx));
            }
            drop(tx);
            let me = self.clone();
            std::thread::spawn(move || me.worker(rx));
            self.emit("team.status", json!({"state": "live", "message": "Listening…", "device": self.engines.whisper_device(&o.model),
                "transcript": self.transcript_path.lock().unwrap().as_ref().map(|p| p.to_string_lossy().into_owned())}));
            while !self.stop_event.wait(0.5) {}
            drop(sources);
            Ok(())
        })();
        if let Err(e) = r {
            let msg = match e.downcast_ref::<JobError>() {
                Some(j) => j.0.clone(),
                None if e.downcast_ref::<st_core::Cancelled>().is_some() => String::new(),
                None => format!("{e:#}"),
            };
            if !msg.is_empty() {
                self.emit("team.error", json!({"message": msg}));
            }
        }
        self.stop();
        self.emit("team.status", json!({"state": "stopped", "message": "Stopped.", "lines": self.lines.load(Ordering::SeqCst),
            "transcript": self.transcript_path.lock().unwrap().as_ref().map(|p| p.to_string_lossy().into_owned())}));
    }

    fn open_transcript(&self) -> Result<()> {
        let folder = self.opts.lock().unwrap().save_dir.join("Live");
        std::fs::create_dir_all(&folder)?;
        let now = chrono::Local::now();
        let path = folder.join(format!("Team_voice_{}.transcript.txt", now.format("%Y-%m-%d_%H-%M")));
        st_core::py::write_text(&path, &format!("Team voice\nStarted {}\n\n", now.format("%Y-%m-%d %H:%M")))?;
        *self.transcript_path.lock().unwrap() = Some(path);
        Ok(())
    }

    /// One source: sound → sentences (and the level meter).
    fn listen(&self, source: usize, user: Option<u64>, sink: Arc<Mutex<Vec<f32>>>, tx: mpsc::Sender<Sentence>) {
        let mut seg = Segmenter::new();
        while !self.stop_event.wait(0.2) {
            let mut more = std::mem::take(&mut *sink.lock().unwrap());
            self.emit("team.level", json!({"source": source, "user": user.map(|u| u.to_string()), "level": level(&more)}));
            match seg.push(&mut more) {
                Ok(done) => {
                    for (audio, s, _) in done {
                        let _ = tx.send(Sentence { source, user, audio, t0: s as f64 / SR as f64, heard: Instant::now() });
                    }
                }
                Err(e) => {
                    self.emit("team.error", json!({"message": format!("Voice detection failed: {e:#}")}));
                    self.stop();
                    return;
                }
            }
        }
    }

    /// Sentences in the order they finished: who said it, then the English.
    fn worker(&self, rx: mpsc::Receiver<Sentence>) {
        while !self.stop_event.is_set() {
            let Ok(s) = rx.recv_timeout(Duration::from_millis(300)) else { continue };
            if let Err(e) = self.caption(&s) {
                if !self.stop_event.is_set() {
                    self.emit("team.error", json!({"message": format!("Translation error: {e:#}"), "fatal": false}));
                }
            }
        }
    }

    /// (name, colour) for a sentence.
    fn speaker(&self, s: &Sentence) -> (String, String) {
        if let Some(u) = s.user {
            return self.person(u);
        }
        let o = self.opts.lock().unwrap();
        if s.source == 1 {
            return (o.my_name.clone(), ME_COLOR.into());
        }
        let mut last = self.last_speaker.lock().unwrap();
        let recent = last.as_ref().filter(|l| l.2.elapsed() < Duration::from_secs(4)).map(|l| (l.0.clone(), l.1.clone()));
        if o.voices.is_empty() {
            return (UNKNOWN.into(), UNKNOWN_COLOR.into());
        }
        if (s.audio.len() as f64) < MIN_ID_S * SR as f64 {
            return recent.unwrap_or((UNKNOWN.into(), UNKNOWN_COLOR.into()));
        }
        match speakers::embedding(&s.audio).ok().as_deref().and_then(|e| identify(&o.voices, e)) {
            Some((v, _)) => {
                *last = Some((v.name.clone(), v.color.clone(), Instant::now()));
                (v.name.clone(), v.color.clone())
            }
            None => (UNKNOWN.into(), UNKNOWN_COLOR.into()),
        }
    }

    fn caption(&self, s: &Sentence) -> Result<()> {
        let t_proc = Instant::now();
        // a cough or a click: too short to be words, and Whisper tends to invent a line for it
        if (s.audio.len() as f64) < MIN_WORDS_S * SR as f64 {
            return Ok(());
        }
        let (lang, use_glossary, champions, names) = {
            let o = self.opts.lock().unwrap();
            let mut names: Vec<String> = o.voices.iter().map(|v| v.name.clone()).collect();
            if o.discord.is_some() {
                // everyone in the call, so Whisper spells their names right
                let ids = self.in_call.lock().unwrap().clone();
                let people = self.people.lock().unwrap();
                names.extend(people.iter().filter(|p| ids.iter().any(|i| i.to_string() == p.id)).map(|p| p.shown().to_string()));
            }
            names.push(o.my_name.clone());
            names.retain(|n| !n.trim().is_empty());
            (o.language.clone(), o.use_glossary, o.champions.clone(), names)
        };
        let model = self.model.lock().unwrap().clone().expect("model loaded");
        let lang = match lang {
            Some(l) => l,
            None => model.detect_language(&s.audio, 1, 0.5)?.0,
        };
        let task = if lang == "en" { "transcribe" } else { "translate" };
        let hot = if use_glossary { self.glossary.lock().unwrap().hotwords(&champions, true, &names) } else { None };
        let mut o = Options::new(&lang, task, hot.as_deref());
        o.beam_size = 5;
        o.vad = false;
        o.without_timestamps = true;
        // a caption that takes 10 s is useless live: one retry at most, and no more text than ~4 words a second
        o.temperatures = &[0.0, 0.4];
        o.max_tokens = (24.0 + 14.0 * s.audio.len() as f64 / SR as f64) as usize;
        let segs = model.transcribe(&s.audio, &o, &|_| {}, &mut |_| {}, &self.stop_event)?;
        let text = {
            let gl = self.glossary.lock().unwrap();
            let t = segs
                .iter()
                .filter_map(|x| {
                    let t = x.text.split_whitespace().collect::<Vec<_>>().join(" ");
                    (!t.is_empty() && !is_junk(x, &t) && !gl.is_prompt_echo(&t, hot.as_deref())).then_some(t)
                })
                .collect::<Vec<_>>()
                .join(" ");
            let t = collapse_repeats(&t);
            if use_glossary { gl.fix(&t, true, &names).0 } else { t }
        };
        // who said it, looked up now rather than before translating, so a rename meanwhile is used
        let (name, color) = self.speaker(s);
        if text.is_empty() || self.stop_event.is_set() {
            return Ok(());
        }
        let n = self.lines.fetch_add(1, Ordering::SeqCst) + 1;
        let now = chrono::Local::now();
        self.emit("team.caption", json!({"id": n, "name": name, "color": color, "text": text, "source": s.source, "lang": lang,
            "lang_name": lang_name(Some(&lang)), "t": (s.t0 * 10.0).round() / 10.0, "clock": now.format("%H:%M:%S").to_string(),
            "delay": (s.heard.elapsed().as_secs_f64() * 10.0).round() / 10.0, "took": (t_proc.elapsed().as_secs_f64() * 100.0).round() / 100.0}));
        if let Some(p) = self.transcript_path.lock().unwrap().as_ref() {
            let _ = append(p, &format!("[{} | +{}]  {name}: {text}\n", now.format("%H:%M:%S"), format_ts(self.started.elapsed().as_secs_f64())));
        }
        Ok(())
    }
}

/// "I'm not sure if I should say it, but I'm not sure if I should say it" -> said once. Whisper repeats
/// itself on short, unclear pieces; a phrase of 3+ words said twice or more in a row is kept once.
pub fn collapse_repeats(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let keys: Vec<String> = words.iter().map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase()).collect();
    let mut out: Vec<&str> = vec![];
    let mut i = 0;
    'outer: while i < words.len() {
        for n in (3..=(words.len() - i) / 2).rev() {
            for gap in 0..=1 {
                let j = i + n + gap;
                if j + n > words.len() || (gap == 1 && !["but", "and", "so", "like"].contains(&keys[i + n].as_str())) {
                    continue;
                }
                if keys[i..i + n] == keys[j..j + n] {
                    // keep the last copy only
                    i = j;
                    continue 'outer;
                }
            }
        }
        out.push(words[i]);
        i += 1;
    }
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::collapse_repeats;

    #[test]
    fn repeats_are_said_once() {
        assert_eq!(collapse_repeats("I'm not sure if I should say it or not, I'm not sure if I should say it or not"), "I'm not sure if I should say it or not");
        assert_eq!(collapse_repeats("I'm not sure if I should say it or not, but I'm not sure if I should say it or not"), "I'm not sure if I should say it or not");
        assert_eq!(collapse_repeats("go go go now"), "go go go now");
        assert_eq!(collapse_repeats("we are going to win we are going to win we are going to win"), "we are going to win");
        assert_eq!(collapse_repeats("Flash is down on their ADC, go now."), "Flash is down on their ADC, go now.");
    }
}

/// Append with Windows line endings (like the live transcript).
fn append(path: &Path, text: &str) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new().append(true).create(true).open(path)?;
    f.write_all(text.replace('\n', "\r\n").as_bytes())
}
