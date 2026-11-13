//! Voice isolation (isolate.py): the UVR MDX-Net "Voc_FT" model strips game sounds, music and alerts
//! so Whisper only hears people. ~6 s windows -> spectrogram -> model -> audio, on the GPU via DirectML.
//! Output matched Python's exactly (134 dB SNR) at 2.4x the speed.

use std::{
    io::{Read, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use ort::{ep, session::Session, value::Tensor};
use realfft::{num_complex::Complex32, ComplexToReal, RealFftPlanner, RealToComplex};
use st_core::{cancel::Cancel, job_err};
use st_media::ffmpeg::pcm;

use crate::gpu;

pub use st_core::models::{uvr_path as model_path, uvr_ready as model_ready, UVR_SIZE as MODEL_SIZE, UVR_URL as MODEL_URL};

const SR: usize = 44100;
const N_FFT: usize = 7680;
const HOP: usize = 1024;
const DIM_F: usize = 3072;
const DIM_T: usize = 256;
const CHUNK: usize = HOP * (DIM_T - 1);
const TRIM: usize = N_FFT / 2;
const GEN: usize = CHUNK - 2 * TRIM;
const NB: usize = N_FFT / 2 + 1;
/// keep a little of the original: smooths separation artifacts Whisper trips on
const BLEED: f32 = 0.15;

pub fn download_model(progress: &dyn Fn(f64)) -> Result<()> {
    if model_ready() {
        return Ok(());
    }
    let dest = model_path();
    std::fs::create_dir_all(dest.parent().unwrap())?;
    let tmp = dest.with_extension("part");
    let mut resp = ureq::get(MODEL_URL).header("User-Agent", "StreamTranslator").call()?;
    let mut body = resp.body_mut().with_config().limit(MODEL_SIZE + 1).reader();
    let mut f = std::fs::File::create(&tmp)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut got = 0u64;
    loop {
        let k = body.read(&mut buf)?;
        if k == 0 {
            break;
        }
        f.write_all(&buf[..k])?;
        got += k as u64;
        progress(got as f64 / MODEL_SIZE as f64);
    }
    drop(f);
    if got != MODEL_SIZE {
        let _ = std::fs::remove_file(&tmp);
        return job_err("The voice isolation model didn't download completely. Try again.");
    }
    std::fs::rename(tmp, dest)?;
    Ok(())
}

struct Uvr {
    sess: Session,
    win: Vec<f32>,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    norm: Vec<f32>,
    gpu: bool,
}

static UVR: Mutex<Option<Uvr>> = Mutex::new(None);

fn open(env: &ort::environment::Environment, model: &Path) -> ort::Result<(Session, bool)> {
    // DirectML: Windows' own GPU layer. Works on any GPU and needs no CUDA libraries.
    let dml = Session::builder(env)?
        .with_execution_providers([ep::DirectML::default().build().error_on_failure()])
        .and_then(|b| b.with_memory_pattern(false))
        .and_then(|b| b.with_parallel_execution(false))
        .map_err(ort::Error::from)
        .and_then(|mut b| b.commit_from_file(model));
    match dml {
        Ok(s) => Ok((s, true)),
        Err(_) => Ok((Session::builder(env)?.commit_from_file(model)?, false)),
    }
}

impl Uvr {
    fn new() -> Result<Uvr> {
        let (sess, gpu) = open(gpu::ort_env()?, &model_path())?;
        let win: Vec<f32> = (0..N_FFT).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N_FFT as f64).cos()) as f32).collect();
        let mut pl = RealFftPlanner::<f32>::new();
        let mut norm = vec![0f32; N_FFT + HOP * (DIM_T - 1)];
        for t in 0..DIM_T {
            for i in 0..N_FFT {
                norm[t * HOP + i] += win[i] * win[i];
            }
        }
        Ok(Uvr { sess, win, fwd: pl.plan_fft_forward(N_FFT), inv: pl.plan_fft_inverse(N_FFT), norm, gpu })
    }

    /// One stereo window [2][CHUNK] -> voice [2][CHUNK]
    fn run_window(&mut self, ch: [&[f32]; 2]) -> Result<[Vec<f32>; 2]> {
        let mut x = vec![0f32; 4 * DIM_F * DIM_T]; // [1, 4, F, T]: L.re, L.im, R.re, R.im
        let mut buf = self.fwd.make_input_vec();
        let mut spec = self.fwd.make_output_vec();
        for c in 0..2 {
            let s = ch[c];
            let n = s.len() as isize;
            let get = |j: isize| -> f32 {
                let j = if j < 0 { -j } else if j >= n { 2 * (n - 1) - j } else { j };
                s[j as usize]
            };
            for t in 0..DIM_T {
                for i in 0..N_FFT {
                    buf[i] = get((t * HOP + i) as isize - TRIM as isize) * self.win[i];
                }
                self.fwd.process(&mut buf, &mut spec)?;
                for f in 3..DIM_F {
                    x[((2 * c) * DIM_F + f) * DIM_T + t] = spec[f].re;
                    x[((2 * c + 1) * DIM_F + f) * DIM_T + t] = spec[f].im;
                }
            }
        }
        let name = self.sess.inputs()[0].name().to_string();
        let out = self.sess.run(ort::inputs![name => Tensor::from_array(([1usize, 4, DIM_F, DIM_T], x))?])?;
        let (_, y) = out[0].try_extract_tensor::<f32>()?;
        let mut res = [vec![0f32; CHUNK], vec![0f32; CHUNK]];
        let mut full = vec![Complex32::new(0.0, 0.0); NB];
        let mut frame = self.inv.make_output_vec();
        let mut acc = vec![0f32; N_FFT + HOP * (DIM_T - 1)];
        for c in 0..2 {
            acc.iter_mut().for_each(|v| *v = 0.0);
            for t in 0..DIM_T {
                for f in 0..DIM_F {
                    full[f] = Complex32::new(y[((2 * c) * DIM_F + f) * DIM_T + t], y[((2 * c + 1) * DIM_F + f) * DIM_T + t]);
                }
                full[0].im = 0.0;
                full[DIM_F..].iter_mut().for_each(|v| *v = Complex32::new(0.0, 0.0));
                self.inv.process(&mut full, &mut frame)?;
                for i in 0..N_FFT {
                    acc[t * HOP + i] += frame[i] / N_FFT as f32 * self.win[i];
                }
            }
            for i in 0..CHUNK {
                res[c][i] = acc[TRIM + i] / self.norm[TRIM + i].max(1e-8);
            }
        }
        Ok(res)
    }

    /// Interleaved stereo at 44.1 kHz -> isolated voice, interleaved.
    fn separate(&mut self, mix: &[f32], progress: &dyn Fn(f64), cancel: &Cancel) -> Result<Vec<f32>> {
        let n = mix.len() / 2;
        let pad = GEN - n % GEN;
        let total = TRIM + n + pad + TRIM;
        let mut ch = [vec![0f32; total], vec![0f32; total]];
        for i in 0..n {
            ch[0][TRIM + i] = mix[2 * i];
            ch[1][TRIM + i] = mix[2 * i + 1];
        }
        let mut out = vec![0f32; 2 * n];
        let windows = (n + pad) / GEN;
        for (k, s) in (0..n + pad).step_by(GEN).enumerate() {
            cancel.check()?;
            let r = self.run_window([&ch[0][s..s + CHUNK], &ch[1][s..s + CHUNK]])?;
            for i in 0..GEN.min(n.saturating_sub(s)) {
                out[2 * (s + i)] = r[0][TRIM + i];
                out[2 * (s + i) + 1] = r[1][TRIM + i];
            }
            progress(((k + 1) as f64 / windows as f64).min(1.0));
        }
        Ok(out)
    }
}

/// "GPU" or "CPU" for the loaded model.
pub fn device() -> &'static str {
    match UVR.lock().unwrap().as_ref() {
        Some(u) if u.gpu => "GPU",
        _ => "CPU",
    }
}

/// Free the model's GPU memory (clip jobs call this when done, to give it back to Whisper).
pub fn release() {
    *UVR.lock().unwrap() = None;
}

/// Match the original loudness and mix a touch of it back in (both 16 kHz mono).
fn finish(voice: &[f32], mix: &[f32]) -> Vec<f32> {
    let n = voice.len().min(mix.len());
    let rms = |x: &[f32]| {
        let r = (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt();
        if r == 0.0 { 1e-9 } else { r }
    };
    let (rv, rm) = (rms(&voice[..n]), rms(&mix[..n]));
    let out: Vec<f32> = (0..n).map(|i| (voice[i] as f64 * (rm / rv) * (1.0 - BLEED as f64) + mix[i] as f64 * BLEED as f64) as f32).collect();
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs()));
    if peak > 0.95 {
        out.iter().map(|v| v / peak * 0.95).collect()
    } else {
        out
    }
}

/// Clip file -> isolated voice as 16 kHz mono (what Whisper wants).
pub fn voice_16k_from_file(path: &Path, progress: &dyn Fn(f64), cancel: &Cancel) -> Result<Vec<f32>> {
    let input = vec!["-i".to_string(), path.to_string_lossy().into_owned()];
    let stereo = pcm(&input, None, SR as u32, 2)?;
    let voice = {
        let mut g = UVR.lock().unwrap();
        if g.is_none() {
            *g = Some(Uvr::new()?);
        }
        g.as_mut().unwrap().separate(&stereo, progress, cancel)?
    };
    let raw: Vec<u8> = voice.iter().flat_map(|v| v.to_le_bytes()).collect();
    let v16 = pcm(&["-f", "f32le", "-ar", "44100", "-ac", "2", "-i", "-"].map(String::from), Some(&raw), 16000, 1)?;
    let m16 = pcm(&input, None, 16000, 1)?;
    Ok(finish(&v16, &m16))
}

/// FFT resampling, like Python's `np.fft.irfft(np.fft.rfft(x), n=n) * (n / len(x))`.
fn resample(x: &[f32], src: usize, dst: usize) -> Vec<f32> {
    let n = (x.len() as f64 * dst as f64 / src as f64).round_ties_even() as usize;
    if n == 0 || x.is_empty() {
        return vec![];
    }
    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(x.len());
    let mut input = x.to_vec();
    let mut spec = fwd.make_output_vec();
    fwd.process(&mut input, &mut spec).expect("fft");
    let inv = planner.plan_fft_inverse(n);
    let mut bins = inv.make_input_vec();
    for (b, s) in bins.iter_mut().zip(&spec) {
        *b = *s;
    }
    // numpy ignores the imaginary part of the first (and, for even n, the last) bin
    bins[0].im = 0.0;
    if n % 2 == 0 {
        let last = bins.len() - 1;
        bins[last].im = 0.0;
    }
    let mut out = inv.make_output_vec();
    inv.process(&mut bins, &mut out).expect("ifft");
    // realfft doesn't normalise: divide by n, then numpy's n / len(x)
    let k = 1.0 / x.len() as f32;
    out.iter().map(|v| v * k).collect()
}

/// Live mode: a 16 kHz mono piece -> isolated 16 kHz mono piece.
pub fn voice_16k_from_16k(x: &[f32], cancel: &Cancel) -> Result<Vec<f32>> {
    let up = resample(x, 16000, SR);
    let stereo: Vec<f32> = up.iter().flat_map(|&v| [v, v]).collect();
    let voice = {
        let mut g = UVR.lock().unwrap();
        if g.is_none() {
            *g = Some(Uvr::new()?);
        }
        g.as_mut().unwrap().separate(&stereo, &|_| {}, cancel)?
    };
    let mono: Vec<f32> = voice.chunks(2).map(|c| (c[0] + c[1]) / 2.0).collect();
    Ok(finish(&resample(&mono, SR, 16000), x))
}
