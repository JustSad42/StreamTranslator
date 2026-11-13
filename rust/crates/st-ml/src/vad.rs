//! Silero VAD v6 (faster-whisper's ONNX model, CPU), speech timestamps and the map back to original
//! times (faster_whisper/vad.py). Output matched Python exactly on the reference clip.

use std::{path::Path, sync::Mutex};

use anyhow::{Context, Result};
use ort::{session::Session, value::Tensor};

use crate::{gpu, mel::SR};

pub struct VadOptions {
    pub threshold: f32,
    pub min_silence_ms: f64,
    pub speech_pad_ms: f64,
    pub min_speech_ms: f64,
}

impl Default for VadOptions {
    fn default() -> Self {
        VadOptions { threshold: 0.5, min_silence_ms: 2000.0, speech_pad_ms: 400.0, min_speech_ms: 0.0 }
    }
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn session() -> Result<std::sync::MutexGuard<'static, Option<Session>>> {
    let mut s = SESSION.lock().unwrap();
    if s.is_none() {
        let env = gpu::ort_env()?;
        let model = gpu::asset("silero_vad_v6.onnx").context("silero_vad_v6.onnx is missing next to the app")?;
        *s = Some(open(env, &model)?);
    }
    Ok(s)
}

fn open(env: &ort::environment::Environment, model: &Path) -> ort::Result<Session> {
    Session::builder(env)?.with_intra_threads(1)?.with_inter_threads(1)?.commit_from_file(model)
}

/// Speech probability per 512-sample window.
pub fn speech_probs(audio: &[f32]) -> Result<Vec<f32>> {
    const N: usize = 512;
    const CTX: usize = 64;
    let mut guard = session()?;
    let sess = guard.as_mut().unwrap();
    let mut padded = audio.to_vec();
    padded.resize(audio.len() + (N - audio.len() % N), 0.0);
    let rows = padded.len() / N;
    // each row = the last 64 samples of the previous row + this row
    let mut batch = vec![0f32; rows * (N + CTX)];
    for r in 0..rows {
        if r > 0 {
            batch[r * (N + CTX)..r * (N + CTX) + CTX].copy_from_slice(&padded[r * N - CTX..r * N]);
        }
        batch[r * (N + CTX) + CTX..(r + 1) * (N + CTX)].copy_from_slice(&padded[r * N..(r + 1) * N]);
    }
    let (mut h, mut c) = (vec![0f32; 128], vec![0f32; 128]);
    let mut out = Vec::with_capacity(rows);
    for start in (0..rows).step_by(10000) {
        let end = (start + 10000).min(rows);
        let x = batch[start * (N + CTX)..end * (N + CTX)].to_vec();
        let res = sess.run(ort::inputs![
            "input" => Tensor::from_array(([end - start, N + CTX], x))?,
            "h" => Tensor::from_array(([1usize, 1, 128], h.clone()))?,
            "c" => Tensor::from_array(([1usize, 1, 128], c.clone()))?,
        ])?;
        out.extend_from_slice(res[0].try_extract_tensor::<f32>()?.1);
        h = res[1].try_extract_tensor::<f32>()?.1.to_vec();
        c = res[2].try_extract_tensor::<f32>()?.1.to_vec();
    }
    Ok(out)
}

/// Speech chunks (start, end) in samples (get_speech_timestamps with unlimited speech length).
pub fn speech_timestamps(audio: &[f32], o: &VadOptions) -> Result<Vec<(usize, usize)>> {
    let probs = speech_probs(audio)?;
    Ok(timestamps_from_probs(&probs, audio.len(), o))
}

pub fn timestamps_from_probs(probs: &[f32], n_samples: usize, o: &VadOptions) -> Vec<(usize, usize)> {
    let win = 512usize;
    let pad = SR as f64 * o.speech_pad_ms / 1000.0;
    let min_silence = SR as f64 * o.min_silence_ms / 1000.0;
    let neg = (o.threshold - 0.15).max(0.01);
    let min_speech = SR as f64 * o.min_speech_ms / 1000.0;
    let (mut triggered, mut temp_end, mut cur_start) = (false, 0usize, 0usize);
    let mut speeches: Vec<(i64, i64)> = vec![];
    for (i, &p) in probs.iter().enumerate() {
        let pos = win * i;
        if p >= o.threshold && temp_end != 0 {
            temp_end = 0;
        }
        if p >= o.threshold && !triggered {
            triggered = true;
            cur_start = pos;
            continue;
        }
        if p < neg && triggered {
            if temp_end == 0 {
                temp_end = pos;
            }
            if ((pos - temp_end) as f64) < min_silence {
                continue;
            }
            if (temp_end as f64 - cur_start as f64) > min_speech {
                speeches.push((cur_start as i64, temp_end as i64));
            }
            temp_end = 0;
            triggered = false;
        }
    }
    if triggered && (n_samples as f64 - cur_start as f64) > min_speech {
        speeches.push((cur_start as i64, n_samples as i64));
    }
    let n = speeches.len();
    for i in 0..n {
        if i == 0 {
            speeches[0].0 = (speeches[0].0 as f64 - pad).max(0.0) as i64;
        }
        if i != n - 1 {
            let gap = speeches[i + 1].0 - speeches[i].1;
            if (gap as f64) < 2.0 * pad {
                speeches[i].1 += gap / 2;
                speeches[i + 1].0 = (speeches[i + 1].0 - gap / 2).max(0);
            } else {
                speeches[i].1 = (speeches[i].1 as f64 + pad).min(n_samples as f64) as i64;
                speeches[i + 1].0 = (speeches[i + 1].0 as f64 - pad).max(0.0) as i64;
            }
        } else {
            speeches[i].1 = (speeches[i].1 as f64 + pad).min(n_samples as f64) as i64;
        }
    }
    speeches.into_iter().map(|(a, b)| (a as usize, b as usize)).collect()
}

/// The speech parts glued together.
pub fn collect(audio: &[f32], chunks: &[(usize, usize)]) -> Vec<f32> {
    chunks.iter().flat_map(|&(a, b)| audio[a..b].iter().cloned()).collect()
}

/// Maps times in the glued speech back to times in the original audio (SpeechTimestampsMap).
pub struct TsMap {
    chunk_end: Vec<usize>,
    silence_before: Vec<f64>,
}

impl TsMap {
    pub fn new(chunks: &[(usize, usize)]) -> Self {
        let (mut prev, mut silent) = (0usize, 0usize);
        let (mut chunk_end, mut silence_before) = (vec![], vec![]);
        for &(s, e) in chunks {
            silent += s - prev;
            prev = e;
            chunk_end.push(e - silent);
            silence_before.push(silent as f64 / SR as f64);
        }
        TsMap { chunk_end, silence_before }
    }

    pub fn original(&self, t: f64, is_end: bool) -> f64 {
        if self.chunk_end.is_empty() {
            return t;
        }
        let sample = (t * SR as f64) as usize;
        let idx = match (is_end, self.chunk_end.iter().position(|&e| e == sample)) {
            (true, Some(i)) => i,
            _ => self.chunk_end.partition_point(|&e| e <= sample).min(self.chunk_end.len() - 1),
        };
        // Python's round(x, 2): correctly rounded on the exact value, which "{:.2}" also does
        format!("{:.2}", self.silence_before[idx] + t).parse().unwrap()
    }
}
