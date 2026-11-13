//! A clip job, start to finish (pipeline.run_job): read the VOD, download the section, transcribe,
//! translate, clean up, write subtitles/transcript/job.json, burn the subtitles into the video.
//! Stage names, log lines and output files are the same as the Python version's.

pub mod engines;

use std::{
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use regex::Regex;
use serde_json::{json, Map, Value};
use st_core::{
    cancel::Cancel,
    dsp::audio_offset,
    glossary::Glossary,
    job_err,
    langs::{lang_name, nllb_code},
    paths, py,
    subs::{write_srt, write_transcript, Segment, TranscriptInfo},
    timefmt::{format_hms_tag, format_ts},
    vod::safe_name,
    JobError,
};
use st_media::{ffmpeg, ytdlp};
use st_ml::{isolate, speakers, whisper};

pub use engines::Engines;

/// Whisper's usual hallucinations on music / game noise / silence.
const JUNK: &[&str] = &[
    "thank you for watching", "thanks for watching", "please subscribe", "like and subscribe",
    "subtitles by", "amara.org", "subtitled by", "transcribed by", "copyright",
    "시청해주셔서 감사합니다", "구독과 좋아요", "mbc 뉴스", "ご視聴ありがとうございました",
    "字幕", "請不吝點贊", "продолжение следует", "sous-titrage", "untertitel",
];

#[derive(Clone, Debug)]
pub struct JobParams {
    pub url: String,
    pub start: f64,
    pub end: f64,
    pub out_dir: PathBuf,
    pub quality: String,
    pub model: String,
    /// None = auto-detect
    pub language: Option<String>,
    pub want_original: bool,
    pub burn: bool,
    pub position: String,
    pub font_size: String,
    /// champions in this game (glossary priority)
    pub champions: Vec<String>,
    pub use_glossary: bool,
    /// strip game audio/music before Whisper
    pub isolate: bool,
    pub label: String,
    /// "two-step" (Whisper writes it down, NLLB translates) or "whisper"
    pub method: String,
    /// NLLB size for two-step: "standard" or "better"
    pub translator: String,
    /// read champions/player names off the video for the glossary
    pub auto_details: bool,
    /// tell speakers apart and colour the others' subtitles
    pub speakers: bool,
    /// folder of a custom faster-whisper model (model = "custom")
    pub custom_model: Option<PathBuf>,
}

impl Default for JobParams {
    fn default() -> Self {
        JobParams {
            url: String::new(),
            start: 0.0,
            end: 0.0,
            out_dir: PathBuf::new(),
            quality: "720p".into(),
            model: "large-v3".into(),
            language: None,
            want_original: false,
            burn: true,
            position: "Above game HUD".into(),
            font_size: "Medium".into(),
            champions: vec![],
            use_glossary: true,
            isolate: false,
            label: String::new(),
            method: "two-step".into(),
            translator: "better".into(),
            auto_details: true,
            speakers: false,
            custom_model: None,
        }
    }
}

/// What the job reports while it runs.
pub struct Callbacks {
    pub stage: Box<dyn Fn(usize, usize, &str) + Send + Sync>,
    /// 0..1, or -1 for "busy"
    pub progress: Box<dyn Fn(f64) + Send + Sync>,
    pub log: Box<dyn Fn(&str) + Send + Sync>,
    /// a finished line: (segment, "orig" | "en")
    pub segment: Box<dyn Fn(&Segment, &str) + Send + Sync>,
    pub cancel: Cancel,
}

impl Default for Callbacks {
    fn default() -> Self {
        Callbacks { stage: Box::new(|_, _, _| {}), progress: Box::new(|_| {}), log: Box::new(|_| {}), segment: Box::new(|_, _| {}), cancel: Cancel::new() }
    }
}

#[derive(Debug)]
pub struct JobResult {
    pub folder: PathBuf,
    pub clip: PathBuf,
    pub srt: PathBuf,
    pub transcript: PathBuf,
    pub subbed: Option<PathBuf>,
    pub language: String,
    pub probability: f64,
    pub segments: usize,
    pub seconds: f64,
    pub meta: Value,
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

// ---------------------------------------------------------------- download cache (download.json)

const DL_FILE: &str = "download.json";

fn downloads(folder: &Path) -> Map<String, Value> {
    std::fs::read(folder.join(DL_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn remember_download(folder: &Path, kind: &str, file: &Path, quality: Option<&str>) -> Result<()> {
    let mut d = downloads(folder);
    let size = std::fs::metadata(file)?.len();
    d.insert(kind.into(), json!({"file": file.file_name().unwrap().to_string_lossy(), "quality": quality, "size": size, "at": py::f(now())}));
    py::write_text(&folder.join(DL_FILE), &py::json_dumps(&Value::Object(d), Some(2), true))?;
    Ok(())
}

/// A finished download of this section from an earlier job (same file size, same quality).
fn cached(folder: &Path, kind: &str, quality: Option<&str>) -> Option<PathBuf> {
    let d = downloads(folder);
    let e = d.get(kind)?;
    let f = folder.join(e["file"].as_str()?);
    if std::fs::metadata(&f).ok()?.len() != e["size"].as_u64()? {
        return None;
    }
    if let Some(q) = quality {
        if e["quality"].as_str() != Some(q) {
            return None;
        }
    }
    Some(f)
}

pub fn write_meta(folder: &Path, meta: &Value) -> std::io::Result<()> {
    py::write_text(&folder.join("job.json"), &py::json_dumps(meta, Some(2), false))
}

pub fn read_meta(folder: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&std::fs::read(folder.join("job.json"))?)?)
}

// ---------------------------------------------------------------- Whisper, then the line filters

pub fn is_junk(seg: &whisper::RawSegment, text: &str) -> bool {
    let low = text.to_lowercase();
    if JUNK.iter().any(|j| low.contains(j)) && low.chars().count() < 80 {
        return true;
    }
    if seg.no_speech_prob > 0.75 && seg.avg_logprob < -0.9 {
        return true;
    }
    static PUNCT: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| Regex::new(r"^[\W_]*$").unwrap());
    PUNCT.is_match(text)
}

/// Whisper, then drop junk lines, hotword echoes and loops (keep at most 2 identical lines in a row),
/// and apply the glossary `fixer`. Returns the lines and how many terms the fixer changed.
#[allow(clippy::too_many_arguments)]
pub fn run_whisper(
    model: &whisper::WhisperModel,
    audio: &[f32],
    language: &str,
    task: &str,
    progress: &(dyn Fn(f64) + Sync),
    on_segment: &dyn Fn(&Segment),
    cancel: &Cancel,
    hotwords: Option<&str>,
    gl: &Glossary,
    fixer: Option<&dyn Fn(&str) -> (String, usize)>,
) -> Result<(Vec<Segment>, usize)> {
    let mut out: Vec<Segment> = vec![];
    let mut repeat = 0;
    let mut fixes = 0;
    let opts = whisper::Options::new(language, task, hotwords);
    model.transcribe(
        audio,
        &opts,
        progress,
        &mut |s| {
            let mut text = s.text.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.is_empty() || is_junk(s, &text) || gl.is_prompt_echo(&text, hotwords) {
                return;
            }
            if let Some(f) = fixer {
                let (t, k) = f(&text);
                text = t;
                fixes += k;
            }
            if out.last().is_some_and(|l| l.text.to_lowercase() == text.to_lowercase()) {
                repeat += 1;
                if repeat >= 2 {
                    return; // a loop: keep the first two copies only
                }
            } else {
                repeat = 0;
            }
            let seg = Segment::new(s.start, s.end, text);
            on_segment(&seg);
            out.push(seg);
        },
        cancel,
    )?;
    progress(1.0);
    Ok((out, fixes))
}

// ---------------------------------------------------------------- the job

pub fn run_job(p: &JobParams, cb: &Callbacks, engines: &Engines) -> Result<JobResult> {
    let t0 = Instant::now();
    if p.end - p.start < 2.0 {
        return job_err("Pick a section at least a few seconds long.");
    }
    let mut two_step = p.method == "two-step";
    let need_video = p.burn;
    let mut stages: Vec<&str> = vec!["Reading VOD info", "Downloading audio", "Loading Whisper"];
    if p.isolate {
        stages.push("Isolating voices");
    }
    stages.push("Detecting language");
    if two_step {
        stages.extend(["Transcribing speech", "Translating to English"]);
    } else {
        stages.push("Translating to English");
        if p.want_original {
            stages.push("Transcribing original language");
        }
    }
    if p.speakers {
        stages.push("Telling speakers apart");
    }
    if need_video {
        stages.extend(["Downloading video", "Burning subtitles into video"]);
    }
    let n = stages.len();
    let step = Mutex::new(0usize);
    let stage = |label: &str| {
        let mut s = step.lock().unwrap();
        *s += 1;
        (cb.stage)(*s, n, label);
        (cb.progress)(-1.0);
    };
    let cancel = &cb.cancel;
    let log = |s: &str| (cb.log)(s);

    stage(stages[0]);
    let info = Arc::new(ytdlp::fetch_info(&p.url)?); // always re-read: Twitch playlist tokens expire
    let end = if info.duration > 0.0 { p.end.min(info.duration) } else { p.end };
    let start = p.start.max(0.0);
    let dur = end - start;
    cancel.check()?;

    let id = safe_name(&info.id);
    let base = format!("{}_{}_{}-{}", safe_name(&info.channel), if info.id.is_empty() { "vod".into() } else { id }, format_hms_tag(start), format_hms_tag(end));
    // like Python's Path: "C:/x" is shown as "C:\x"
    let folder = PathBuf::from(p.out_dir.to_string_lossy().replace('/', "\\")).join(&base);
    std::fs::create_dir_all(&folder)?;
    let audio_file = folder.join(format!("{base}.m4a"));
    let video_file = folder.join(format!("{base}.mp4"));
    log(&format!("Saving to {}", folder.display()));

    // The video downloads in the background while Whisper works on the sound.
    let stop_video = Cancel::new();
    let vprog = Arc::new(Mutex::new(0.0f64));
    let have_video = if need_video { cached(&folder, "video", Some(&p.quality)) } else { None };
    let mut video_thread = None;
    if need_video && have_video.is_some() {
        log(&format!("Reusing the {} video downloaded earlier.", p.quality));
    } else if need_video {
        let (info, folder, video_file, quality) = (info.clone(), folder.clone(), video_file.clone(), p.quality.clone());
        let c = cancel.either(&stop_video);
        let vp = vprog.clone();
        video_thread = Some(std::thread::spawn(move || -> Result<()> {
            let prog = move |f: f64| *vp.lock().unwrap() = f;
            ffmpeg::download_section(&info, &quality, start, end, &video_file, &prog, &c, false)?;
            remember_download(&folder, "video", &video_file, Some(&quality))
        }));
    }

    // champions and player names, read off the video while it downloads
    let gl = Arc::new(Glossary::load(&paths::data_dir()));
    let mut details: Value = json!({});
    let mut det_rx = None;
    let det_file = folder.join("details.json");
    if p.auto_details && p.use_glossary {
        match std::fs::read(&det_file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            Some(d) => details = d, // read on an earlier run
            None => {
                let (tx, rx) = mpsc::channel();
                let (info, gl, c, det_file) = (info.clone(), gl.clone(), cancel.clone(), det_file.clone());
                std::thread::spawn(move || {
                    let r = st_vision::game_details(&info, start, end, &gl, &c, 4);
                    if let Ok(found) = &r {
                        if !c.is_set() {
                            let _ = std::fs::write(&det_file, py::json_dumps(found, None, false));
                        }
                    }
                    let _ = tx.send(r.map_err(|e| e.to_string()));
                });
                det_rx = Some(rx);
            }
        }
    }

    let body = (|| -> Result<JobResult> {
        stage("Downloading audio");
        if cached(&folder, "audio", None).is_some() {
            log("Reusing the audio downloaded earlier.");
        } else {
            let bg = if video_thread.is_some() { format!(", the {} video in the background.", p.quality) } else { ".".into() };
            log(&format!("Downloading {} - {} ({}): sound first{bg}", format_ts(start), format_ts(end), format_ts(dur)));
            ffmpeg::download_section(&info, &p.quality, start, end, &audio_file, &*cb.progress, cancel, true)?;
            remember_download(&folder, "audio", &audio_file, None)?;
        }

        let thumb = folder.join("thumb.jpg");
        if !thumb.exists() {
            if let Ok(b) = ffmpeg::grab_frame(&info, start + dur * 0.3, 480) {
                let _ = std::fs::write(&thumb, b);
            }
        }

        stage("Loading Whisper");
        if whisper::local_model(&p.model, p.custom_model.as_deref()).is_none() {
            log(&format!("First run: downloading the Whisper {} model. This only happens once.", p.model));
        }
        let model = engines.whisper(&p.model, p.custom_model.as_deref(), &log, &*cb.progress, cancel)?;
        cancel.check()?;

        let raw = ffmpeg::decode_audio(&audio_file)?;
        let audio = if p.isolate {
            stage("Isolating voices");
            if !isolate::model_ready() {
                log("First use: downloading the voice isolation model (66 MB).");
                isolate::download_model(&*cb.progress)?;
            }
            let r = isolate::voice_16k_from_file(&audio_file, &*cb.progress, cancel);
            if r.is_ok() {
                log(&format!("Game sound and music removed ({}).", isolate::device()));
            }
            isolate::release(); // give the GPU memory back to Whisper
            r?
        } else {
            raw.clone()
        };

        stage("Detecting language");
        let (lang, prob) = match &p.language {
            Some(l) if !l.is_empty() => {
                log(&format!("Language set manually: {}", lang_name(Some(l))));
                (l.clone(), 1.0)
            }
            _ => {
                let (lang, prob, top) = model.detect_language(&audio, 8, 0.7)?;
                if !top.is_empty() {
                    let guess: Vec<String> = top.iter().map(|(c, p)| format!("{} {:.0}%", lang_name(Some(c)), p * 100.0)).collect();
                    log(&format!("Language guess: {}", guess.join(", ")));
                }
                log(&format!("Detected language: {} ({:.0}%)", lang_name(Some(&lang)), prob * 100.0));
                if prob < 0.5 {
                    log("Low confidence. If the result looks wrong, pick the language manually and run again.");
                }
                (lang, prob as f64)
            }
        };
        cancel.check()?;

        if let Some(rx) = &det_rx {
            // don't hold up a short clip; long ones finish downloading first
            match rx.recv_timeout(Duration::from_secs(20)) {
                Ok(Ok(found)) => details = found,
                Ok(Err(e)) => details = json!({"error": e}),
                Err(_) => log("Still reading champions/players from the video; skipping that for this clip."),
            }
        }
        let strs = |k: &str| -> Vec<String> { details[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
        let (dchamps, names) = (strs("champions"), strs("players"));
        if !dchamps.is_empty() || !names.is_empty() {
            log(&format!("Read from the game: {}", dchamps.iter().chain(&names).cloned().collect::<Vec<_>>().join(", ")));
        }
        let mut champs: Vec<String> = vec![];
        for c in p.champions.iter().chain(&dchamps) {
            if !champs.contains(c) {
                champs.push(c.clone());
            }
        }
        let fix_en = |t: &str| gl.fix(t, true, &names);
        let fix_orig = |t: &str| gl.fix(t, false, &names);
        let (fix_en, fix_orig): (Option<&dyn Fn(&str) -> (String, usize)>, Option<&dyn Fn(&str) -> (String, usize)>) =
            if p.use_glossary { (Some(&fix_en), Some(&fix_orig)) } else { (None, None) };

        if two_step && lang != "en" && nllb_code(&lang).is_none() {
            log(&format!("The text translator doesn't cover {}, so Whisper translates directly.", lang_name(Some(&lang))));
            two_step = false;
        }

        let mut original: Option<Vec<Segment>> = None;
        let (mut fixes, mut slang) = (0usize, 0usize);
        let english: Vec<Segment>;
        let seg_orig = |s: &Segment| (cb.segment)(s, "orig");
        let seg_en = |s: &Segment| (cb.segment)(s, "en");
        if two_step {
            stage("Transcribing speech");
            let hot = if p.use_glossary { gl.hotwords(&champs, false, &names) } else { None };
            let (orig, _) = run_whisper(&model, &audio, &lang, "transcribe", &*cb.progress, &seg_orig, cancel, hot.as_deref(), &gl, fix_orig)?;
            cancel.check()?;
            stage("Translating to English");
            if lang == "en" {
                english = orig;
            } else {
                let tr = engines.translator(&p.translator, &log, &*cb.progress, cancel)?;
                let src: Vec<String> = orig
                    .iter()
                    .map(|s| {
                        if p.use_glossary {
                            let (t, k) = gl.prepare(&s.text, Some(&lang));
                            slang += k;
                            t
                        } else {
                            s.text.clone()
                        }
                    })
                    .collect();
                let outs = tr.translate(&src, &lang, 4)?;
                let mut en = vec![];
                for (s, t) in orig.iter().zip(outs) {
                    let t = match fix_en {
                        Some(f) => {
                            let (t, k) = f(&t);
                            fixes += k;
                            t
                        }
                        None => t,
                    };
                    let seg = Segment::new(s.start, s.end, t);
                    seg_en(&seg);
                    en.push(seg);
                }
                english = en;
                original = Some(orig);
                (cb.progress)(1.0);
                log(&format!(
                    "Translated by Whisper {} + NLLB {} ({}).",
                    p.model,
                    if tr.size == "better" { "1.3B" } else { "600M" },
                    tr.device
                ));
            }
        } else {
            let hot = if p.use_glossary { gl.hotwords(&champs, true, &names) } else { None };
            if let Some(h) = &hot {
                log(&format!("League glossary on ({} terms in Whisper's prompt).", h.split(',').count()));
            }
            stage("Translating to English");
            let task = if lang == "en" { "transcribe" } else { "translate" };
            if lang == "en" {
                log("Speech is already English, so it's transcribed without translating.");
            }
            let (en, k) = run_whisper(&model, &audio, &lang, task, &*cb.progress, &seg_en, cancel, hot.as_deref(), &gl, fix_en)?;
            english = en;
            fixes = k;
            if p.want_original {
                stage("Transcribing original language");
                original = Some(run_whisper(&model, &audio, &lang, "transcribe", &*cb.progress, &seg_orig, cancel, None, &gl, fix_orig)?.0);
            }
        }
        let mut english = english;
        log(&format!(
            "{} English lines.{}{}",
            english.len(),
            if fixes > 0 { format!(" Glossary fixed {fixes} terms.") } else { String::new() },
            if slang > 0 { format!(" {slang} slang phrases explained before translating.") } else { String::new() }
        ));

        let mut n_speakers = 1usize;
        if p.speakers && !english.is_empty() {
            stage("Telling speakers apart");
            if !speakers::model_ready() {
                log("First use: downloading the speaker model (26 MB).");
                speakers::download_model(&*cb.progress)?;
            }
            let spans: Vec<(f64, f64)> = english.iter().map(|s| (s.start, s.end)).collect();
            let ids = speakers::label(&audio, &spans, &*cb.progress, cancel)?;
            for (s, k) in english.iter_mut().zip(&ids) {
                s.speaker = *k;
            }
            if let Some(o) = original.as_mut().filter(|o| o.len() == ids.len()) {
                for (s, k) in o.iter_mut().zip(&ids) {
                    s.speaker = *k;
                }
            }
            let mut uniq = ids.clone();
            uniq.sort();
            uniq.dedup();
            n_speakers = uniq.len();
            log(&format!(
                "{n_speakers} speakers.{}",
                if n_speakers > 1 { " Others are shown in colour; the main speaker in white." } else { " Everything was said by one person." }
            ));
            if n_speakers == 1 {
                english.iter_mut().for_each(|s| s.speaker = 0);
                if let Some(o) = original.as_mut() {
                    o.iter_mut().for_each(|s| s.speaker = 0);
                }
            }
        }

        // The video can start a few seconds before the requested time (it has to begin on a keyframe).
        // Find how much earlier by matching its sound against the audio we transcribed.
        let mut shift = 0.0;
        if need_video {
            stage("Downloading video");
            if let Some(h) = video_thread.take() {
                if !h.is_finished() {
                    log("Waiting for the video download to finish…");
                }
                while !h.is_finished() {
                    (cb.progress)(*vprog.lock().unwrap());
                    cancel.check()?;
                    std::thread::sleep(Duration::from_millis(300));
                }
                h.join().map_err(|_| JobError("The video download crashed.".into()))??;
            }
            (cb.progress)(1.0);
            shift = match ffmpeg::decode_audio(&video_file) {
                Ok(v) => format!("{:.3}", audio_offset(&raw, &v, 15.0)).parse().unwrap(),
                Err(_) => 0.0,
            };
            if f64::abs(shift) >= 0.05 {
                log(&format!("The video starts {:.1} s {}; subtitles moved to match.", shift.abs(), if shift > 0.0 { "earlier" } else { "later" }));
            } else {
                shift = 0.0;
            }
        }

        if let Some(o) = &original {
            if lang != "en" {
                write_srt(o, &folder.join(format!("{base}.{lang}.srt")), shift)?;
            }
        }
        let srt = folder.join(format!("{base}.en.srt"));
        write_srt(&english, &srt, shift)?;
        let transcript = folder.join(format!("{base}.transcript.txt"));
        let orig_for_transcript = original.as_deref().filter(|_| (p.want_original || two_step) && lang != "en");
        let ti = TranscriptInfo { info: &info, start, end, lang: &lang, prob, model: &p.model };
        write_transcript(&transcript, &ti, &english, orig_for_transcript, shift)?;

        let clip = if need_video { video_file.clone() } else { audio_file.clone() };
        let fname = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        let mut meta = json!({
            "vod_url": info.url, "vod_id": info.id, "site": info.site, "title": info.title, "channel": info.channel,
            "start": py::f(start), "end": py::f(end), "language": lang, "probability": py::f(prob), "model": p.model,
            "method": if two_step { "two-step" } else { "whisper" }, "translator": if two_step { Some(&p.translator) } else { None },
            "position": p.position, "size": p.font_size, "lines": english.len(), "created": py::f(now()),
            "clip": fname(&clip), "audio_only": !need_video, "srt": fname(&srt), "transcript": fname(&transcript),
            "thumb": if thumb.exists() { Some("thumb.jpg") } else { None }, "subbed": null, "shift": py::f(shift),
            "speakers": n_speakers, "champions": champs, "players": names,
        });
        write_meta(&folder, &meta)?;

        let mut subbed = None;
        if need_video {
            stage("Burning subtitles into video");
            let out = folder.join(format!("{base}.subbed.mp4"));
            let audio_dur = audio.len() as f64 / 16000.0;
            ffmpeg::burn_subtitles(&video_file, &srt, &out, &p.position, &p.font_size, audio_dur + shift, &*cb.progress, &log, cancel)?;
            meta["subbed"] = fname(&out).into();
            write_meta(&folder, &meta)?;
            subbed = Some(out);
        }
        (cb.progress)(1.0);
        Ok(JobResult {
            folder: folder.clone(),
            clip,
            srt,
            transcript,
            subbed,
            language: lang,
            probability: prob,
            segments: english.len(),
            seconds: t0.elapsed().as_secs_f64(),
            meta,
        })
    })();
    if let Some(h) = video_thread {
        // the job failed or was cancelled: stop the background download
        stop_video.set();
        let _ = h.join();
        let _ = std::fs::remove_file(&video_file);
    }
    body
}

// ---------------------------------------------------------------- VOD sync (network side of sync.match)

/// Find their VOD covering my section and map the times across. `theirs`: a Twitch channel or VOD link.
pub fn sync_match(mine: &st_core::vod::VodInfo, my_start: f64, my_end: f64, theirs: &str) -> Result<(st_core::vod::VodInfo, st_core::sync::Matched)> {
    use st_core::sync::{classify, map_times, vod_covering, Kind};
    let Some(my_ts) = mine.start_ts else {
        return job_err("Twitch didn't say when your VOD started, so it can't be synced.");
    };
    let (t0, t1) = (my_ts + my_start, my_ts + my_end);
    let (kind, value) = classify(theirs)?;
    let info = match kind {
        Kind::Channel => {
            let vods = ytdlp::list_vods(&value, 20)?;
            let Some(hit) = vod_covering(&vods, (t0 + t1) / 2.0).or_else(|| vod_covering(&vods, t0)) else {
                let when = chrono::DateTime::from_timestamp(t0 as i64, 0).map(|d| d.with_timezone(&chrono::Local).format("%b %d %H:%M").to_string()).unwrap_or_default();
                return job_err(format!("{value} wasn't live at {when} (checked their last {} VODs).", vods.len()));
            };
            ytdlp::fetch_info(&hit.url)?
        }
        Kind::Vod => ytdlp::fetch_info(&value)?,
        Kind::Url if !theirs.trim().to_lowercase().starts_with("http") => return job_err(format!("Couldn't find a Twitch channel called \"{}\".", theirs.trim())),
        Kind::Url => return job_err("For syncing, give their Twitch channel name or a Twitch VOD link."),
    };
    if info.start_ts.is_none() {
        return job_err("Twitch didn't say when their VOD started, so it can't be synced.");
    }
    let m = map_times(mine, my_start, my_end, &info)?;
    Ok((info, m))
}

/// Burn the (edited) subtitles into the clip again, without re-translating (pipeline.reburn).
pub fn reburn(folder: &Path, position: &str, size: &str, cb: &Callbacks) -> Result<PathBuf> {
    let mut meta = read_meta(folder)?;
    let s = |k: &str| meta[k].as_str().unwrap_or("").to_string();
    let (clip, srt) = (folder.join(s("clip")), folder.join(s("srt")));
    if meta["audio_only"].as_bool().unwrap_or(false) {
        return job_err("This job only downloaded the audio (subtitles only), so there's no video to burn into. Translate the section again with 'Burn subtitles into video' on.");
    }
    if !clip.exists() {
        return job_err("The original clip file is missing, so the subtitles can't be burned again.");
    }
    (cb.stage)(1, 1, "Burning subtitles into video");
    let out = folder.join(format!("{}.subbed.mp4", clip.file_stem().unwrap().to_string_lossy()));
    let dur = (meta["end"].as_f64().unwrap_or(0.0) - meta["start"].as_f64().unwrap_or(0.0)).max(1.0);
    let log = |m: &str| (cb.log)(m);
    ffmpeg::burn_subtitles(&clip, &srt, &out, position, size, dur, &*cb.progress, &log, &cb.cancel)?;
    meta["subbed"] = out.file_name().unwrap().to_string_lossy().into_owned().into();
    meta["position"] = position.into();
    meta["size"] = size.into();
    meta["edited"] = py::f(now());
    write_meta(folder, &meta)?;
    Ok(out)
}
