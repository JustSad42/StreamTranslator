//! Whisper on CTranslate2: a port of faster-whisper's transcribe() and detect_language() with the
//! settings StreamTranslator uses (beam 5, VAD, no conditioning on previous text, hotword prompt).
//! On the reference clip this gave the same text and timings as faster-whisper (75/78 lines identical).

use std::{io::Write, path::Path};

use anyhow::{anyhow, Context, Result};
use ct2rs_sys::{ComputeType, Config, Device, StorageView, Whisper, WhisperOptions};
use st_core::cancel::Cancel;

use crate::{
    gpu,
    mel::{Mel, N_FRAMES, SR},
    vad::{self, TsMap, VadOptions},
};

const TIME_PRECISION: f64 = 0.02;
const TIME_PER_FRAME: f64 = 0.01;
const INPUT_STRIDE: usize = 2;
const MAX_LENGTH: usize = 448;

pub use st_core::models::{local_whisper as local_model, whisper_repo as repo_for};
use st_core::models::WHISPER_FILES as MODEL_FILES;

struct Tok {
    hf: tokenizers::Tokenizer,
    eot: usize,
    sot: usize,
    sot_prev: usize,
    sot_lm: usize,
    no_speech: usize,
    transcribe: usize,
    translate: usize,
    ts_begin: usize,
}

impl Tok {
    fn load(dir: &Path) -> Result<Tok> {
        let hf = tokenizers::Tokenizer::from_file(dir.join("tokenizer.json")).map_err(|e| anyhow!("{e}"))?;
        let id = |t: &str| hf.token_to_id(t).map(|i| i as usize);
        let need = |t: &str| id(t).with_context(|| format!("tokenizer has no {t}"));
        Ok(Tok {
            eot: need("<|endoftext|>")?,
            sot: need("<|startoftranscript|>")?,
            sot_prev: need("<|startofprev|>")?,
            sot_lm: need("<|startoflm|>")?,
            no_speech: id("<|nospeech|>").or_else(|| id("<|nocaptions|>")).context("tokenizer has no no-speech token")?,
            transcribe: need("<|transcribe|>")?,
            translate: need("<|translate|>")?,
            ts_begin: need("<|notimestamps|>")? + 1,
            hf,
        })
    }
    fn encode(&self, text: &str) -> Vec<usize> {
        self.hf.encode(text, false).map(|e| e.get_ids().iter().map(|&i| i as usize).collect()).unwrap_or_default()
    }
    fn decode(&self, ids: &[usize]) -> String {
        let ids: Vec<u32> = ids.iter().filter(|&&t| t < self.eot).map(|&t| t as u32).collect();
        self.hf.decode(&ids, true).unwrap_or_default()
    }
    fn strs(&self, ids: &[usize]) -> Vec<String> {
        ids.iter().map(|&i| self.hf.id_to_token(i as u32).unwrap_or_default()).collect()
    }

    /// Tokens suppressed while decoding: speaker tags, music symbols... (non_speech_tokens +
    /// get_suppressed_tokens).
    fn suppressed(&self) -> Vec<i32> {
        let mut symbols: Vec<String> = "\"#()*+/:;<=>@[\\]^_`{|}~「」『』".chars().map(String::from).collect();
        symbols.extend("<< >> <<< >>> -- --- -( -[ (' (\" (( )) ((( ))) [[ ]] {{ }} ♪♪ ♪♪♪".split(' ').map(String::from));
        let misc: Vec<String> = "♩♪♫♬♭♮♯".chars().map(String::from).collect();
        let mut result: std::collections::BTreeSet<usize> = [self.encode(" -")[0], self.encode(" '")[0]].into();
        for sym in symbols.iter().chain(&misc) {
            for toks in [self.encode(sym), self.encode(&format!(" {sym}"))] {
                if !toks.is_empty() && (toks.len() == 1 || misc.contains(sym)) {
                    result.insert(toks[0]);
                }
            }
        }
        result.extend([self.transcribe, self.translate, self.sot, self.sot_prev, self.sot_lm, self.no_speech]);
        result.into_iter().map(|t| t as i32).collect()
    }
}

/// One stretch of speech as Whisper produced it (times in the original audio).
#[derive(Clone, Debug)]
pub struct RawSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub no_speech_prob: f32,
    pub avg_logprob: f64,
    pub temperature: f32,
}

pub struct Options<'a> {
    pub language: &'a str,
    /// "transcribe" or "translate"
    pub task: &'a str,
    pub beam_size: usize,
    pub hotwords: Option<&'a str>,
    pub no_speech_threshold: f32,
    pub min_silence_ms: f64,
    /// false: Whisper hears the whole audio (live mode cuts its own pieces)
    pub vad: bool,
    pub without_timestamps: bool,
}

impl<'a> Options<'a> {
    pub fn new(language: &'a str, task: &'a str, hotwords: Option<&'a str>) -> Self {
        Options { language, task, beam_size: 5, hotwords, no_speech_threshold: 0.6, min_silence_ms: 500.0, vad: true, without_timestamps: false }
    }
}

pub struct WhisperModel {
    model: Whisper,
    tok: Tok,
    n_mels: usize,
    suppress: Vec<i32>,
    /// "GPU" or "CPU"
    pub device: &'static str,
}

// CTranslate2 models are thread-safe for concurrent calls.
unsafe impl Send for WhisperModel {}
unsafe impl Sync for WhisperModel {}

fn config(gpu: bool) -> Config {
    let mut c = Config::default();
    if gpu {
        c.device = Device::CUDA;
        c.compute_type = ComputeType::FLOAT16;
        c.device_indices = vec![0];
    } else {
        c.device = Device::CPU;
        c.compute_type = ComputeType::INT8;
    }
    c
}

impl WhisperModel {
    /// Load a model, downloading it into the shared Hugging Face cache first if needed.
    pub fn load(name: &str, custom: Option<&Path>, log: &dyn Fn(&str), progress: &dyn Fn(f64), cancel: &Cancel) -> Result<WhisperModel> {
        let dir = match local_model(name, custom) {
            Some(d) => d,
            None if name == "custom" => return st_core::job_err("The custom Whisper model folder is missing or has no model.bin."),
            None => {
                let repo = repo_for(name).with_context(|| format!("Unknown Whisper model: {name}"))?;
                st_media::hub::snapshot(repo, MODEL_FILES, progress, cancel)?
            }
        };
        let n_mels = std::fs::read(dir.join("preprocessor_config.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v["feature_size"].as_u64())
            .unwrap_or(80) as usize;
        let tok = Tok::load(&dir)?;
        let suppress = tok.suppressed();
        let (model, device) = if gpu::cuda_ready() {
            match Whisper::new(&dir, config(true)) {
                Ok(m) => (m, "GPU"),
                Err(e) => {
                    log(&format!("GPU load failed ({e}). Falling back to CPU, which is much slower."));
                    (Whisper::new(&dir, config(false))?, "CPU")
                }
            }
        } else {
            log("No usable NVIDIA GPU libraries found. Whisper runs on the CPU, which is much slower.");
            (Whisper::new(&dir, config(false))?, "CPU")
        };
        Ok(WhisperModel { model, tok, n_mels, suppress, device })
    }

    pub fn is_multilingual(&self) -> bool {
        self.model.is_multilingual()
    }

    fn encode(&self, mel: &Mel, seek: usize, size: usize) -> Result<StorageView<'static>> {
        let mut w = mel.window(seek, size);
        let sv = StorageView::new(&[1, self.n_mels, N_FRAMES], &mut w, Device::CPU)?;
        self.model.encode(&sv, false)
    }

    /// Language code, its probability, and the top 3 guesses (VAD on, up to `segments` windows,
    /// stops early once a guess is above `threshold`).
    pub fn detect_language(&self, audio: &[f32], segments: usize, threshold: f32) -> Result<(String, f32, Vec<(String, f32)>)> {
        let chunks = vad::speech_timestamps(audio, &VadOptions::default())?;
        let speech = vad::collect(audio, &chunks);
        let speech = &speech[..speech.len().min(segments * N_FRAMES * 160)];
        let mel = Mel::new(speech, self.n_mels);
        let frames = mel.frames.min(segments * N_FRAMES);
        let mut seen: Vec<(String, Vec<f32>)> = vec![];
        let mut all: Vec<(String, f32)> = vec![];
        let mut i = 0;
        loop {
            let enc = self.encode(&mel, i, N_FRAMES.min(frames.saturating_sub(i)))?;
            let res = self.model.detect_language(&enc)?.remove(0);
            all = res.iter().map(|d| (d.language.trim_start_matches("<|").trim_end_matches("|>").to_string(), d.probability)).collect();
            let (lang, prob) = all[0].clone();
            if prob > threshold {
                return Ok((lang, prob, all.into_iter().take(3).collect()));
            }
            match seen.iter_mut().find(|e| e.0 == lang) {
                Some(e) => e.1.push(prob),
                None => seen.push((lang, vec![prob])),
            }
            i += N_FRAMES;
            if i >= frames {
                break;
            }
        }
        let best = seen.iter().fold(&seen[0], |b, e| if e.1.len() > b.1.len() { e } else { b });
        let prob = best.1.iter().cloned().fold(f32::MIN, f32::max);
        Ok((best.0.clone(), prob, all.into_iter().take(3).collect()))
    }

    /// Transcribe (or translate) 16 kHz audio. Segments are passed to `on_segment` as they're found.
    pub fn transcribe(&self, audio: &[f32], o: &Options, progress: &dyn Fn(f64), on_segment: &mut dyn FnMut(&RawSegment), cancel: &Cancel) -> Result<Vec<RawSegment>> {
        let duration = audio.len() as f64 / SR as f64;
        let chunks = if o.vad { vad::speech_timestamps(audio, &VadOptions { min_silence_ms: o.min_silence_ms, ..Default::default() })? } else { vec![(0, audio.len())] };
        let speech = vad::collect(audio, &chunks);
        let map = TsMap::new(&chunks);
        let mel = Mel::new(&speech, self.n_mels);
        let lang_tok = self.tok.hf.token_to_id(&format!("<|{}|>", o.language)).with_context(|| format!("Whisper doesn't know the language {}", o.language))? as usize;
        let task = if o.task == "translate" { self.tok.translate } else { self.tok.transcribe };
        // condition_on_previous_text=False keeps the prompt fixed: sot_prev + hotwords + sot sequence
        let mut prompt_ids = vec![];
        if let Some(h) = o.hotwords.filter(|h| !h.trim().is_empty()) {
            let mut hot = self.tok.encode(&format!(" {}", h.trim()));
            if hot.len() >= MAX_LENGTH / 2 {
                hot.truncate(MAX_LENGTH / 2 - 1);
            }
            prompt_ids.push(self.tok.sot_prev);
            prompt_ids.extend(hot);
        }
        prompt_ids.extend([self.tok.sot, lang_tok, task]);
        if o.without_timestamps {
            prompt_ids.push(self.tok.ts_begin - 1);
        }
        let prompt = self.tok.strs(&prompt_ids);

        let content_frames = mel.frames;
        let mut seek = 0usize;
        let mut out = vec![];
        while seek < content_frames {
            cancel.check()?;
            let time_offset = seek as f64 * TIME_PER_FRAME;
            let size = N_FRAMES.min(content_frames - seek);
            let seg_dur = size as f64 * TIME_PER_FRAME;
            let enc = self.encode(&mel, seek, size)?;
            let (ids, avg, temp, nsp) = self.generate_with_fallback(&enc, &prompt, o)?;
            if nsp > o.no_speech_threshold && avg <= -1.0 {
                seek += size;
                continue;
            }
            let tb = self.tok.ts_begin;
            let single_end = ids.len() >= 2 && ids[ids.len() - 2] < tb && tb <= ids[ids.len() - 1];
            let consecutive: Vec<usize> = (1..ids.len()).filter(|&i| ids[i] >= tb && ids[i - 1] >= tb).collect();
            let mut current = vec![];
            if !consecutive.is_empty() {
                let mut slices = consecutive;
                if single_end {
                    slices.push(ids.len());
                }
                let mut last = 0;
                for s in slices {
                    let sl = &ids[last..s];
                    current.push((time_offset + (sl[0] - tb) as f64 * TIME_PRECISION, time_offset + (sl[sl.len() - 1] - tb) as f64 * TIME_PRECISION, sl.to_vec()));
                    last = s;
                }
                if single_end {
                    seek += size;
                } else {
                    seek += (ids[last - 1] - tb) * INPUT_STRIDE;
                }
            } else {
                let mut dur = seg_dur;
                if let Some(&l) = ids.iter().filter(|&&t| t >= tb).last() {
                    if l != tb {
                        dur = (l - tb) as f64 * TIME_PRECISION;
                    }
                }
                current.push((time_offset, time_offset + dur, ids.clone()));
                seek += size;
            }
            for (st, en, toks) in current {
                let text = self.tok.decode(&toks);
                if st == en || text.trim().is_empty() {
                    continue;
                }
                let seg = RawSegment { start: map.original(st, false), end: map.original(en, true), text, no_speech_prob: nsp, avg_logprob: avg, temperature: temp };
                progress(if duration > 0.0 { (seg.end / duration).min(1.0) } else { 0.0 });
                on_segment(&seg);
                out.push(seg);
            }
        }
        progress(1.0);
        Ok(out)
    }

    /// Beam search, retried with sampling at rising temperatures when the result looks like a loop
    /// (compression ratio > 2.4) or is unlikely (average log-probability < -1).
    fn generate_with_fallback(&self, enc: &StorageView, prompt: &[String], o: &Options) -> Result<(Vec<usize>, f64, f32, f32)> {
        let mut results: Vec<(Vec<usize>, f64, f32, f64, f32)> = vec![];
        for &temp in &[0.0f32, 0.2, 0.4, 0.6, 0.8, 1.0] {
            let mut w = WhisperOptions::default();
            w.length_penalty = 1.0;
            w.repetition_penalty = 1.0;
            w.no_repeat_ngram_size = 0;
            w.max_length = MAX_LENGTH;
            w.return_scores = true;
            w.return_no_speech_prob = true;
            w.suppress_blank = true;
            w.suppress_tokens = self.suppress.clone();
            w.max_initial_timestamp_index = 50;
            if temp > 0.0 {
                w.beam_size = 1;
                w.num_hypotheses = 5;
                w.sampling_topk = 0;
                w.sampling_temperature = temp;
            } else {
                w.beam_size = o.beam_size;
                w.patience = 1.0;
                w.num_hypotheses = 1;
            }
            let r = self.model.generate(enc, &[prompt.to_vec()], &w)?.remove(0);
            let ids = r.sequences_ids[0].clone();
            let avg = r.scores[0] as f64 * ids.len() as f64 / (ids.len() + 1) as f64;
            let cr = compression_ratio(self.tok.decode(&ids).trim());
            let mut fallback = cr > 2.4 || avg < -1.0;
            if r.no_speech_prob > o.no_speech_threshold && avg < -1.0 {
                fallback = false; // silence
            }
            results.push((ids, avg, temp, cr, r.no_speech_prob));
            if !fallback {
                let (ids, avg, temp, _, nsp) = results.pop().unwrap();
                return Ok((ids, avg, temp, nsp));
            }
        }
        let below: Vec<_> = results.iter().filter(|r| r.3 <= 2.4).cloned().collect();
        let pool = if below.is_empty() { results } else { below };
        let best = pool.into_iter().fold(None::<(Vec<usize>, f64, f32, f64, f32)>, |b, r| match b {
            Some(b) if b.1 >= r.1 => Some(b),
            _ => Some(r),
        });
        let (ids, avg, _, _, nsp) = best.unwrap();
        Ok((ids, avg, 1.0, nsp))
    }
}

fn compression_ratio(text: &str) -> f64 {
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    e.write_all(text.as_bytes()).unwrap();
    text.len() as f64 / e.finish().unwrap().len() as f64
}
