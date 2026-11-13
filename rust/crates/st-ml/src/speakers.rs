//! Telling speakers apart (speakers.py): each line gets a voice fingerprint from WeSpeaker ResNet34
//! (ONNX, CPU), lines with close fingerprints are grouped, the person who talks most is speaker 1.
//! Fingerprints matched Python's exactly (cosine 1.000000).

use std::{
    io::{Read, Write},
    sync::Mutex,
};

use anyhow::Result;
use ort::{session::Session, value::Tensor};
use realfft::RealFftPlanner;
use st_core::{cancel::Cancel, cluster, job_err};

use crate::gpu;

pub use st_core::models::{wespeaker_path as model_path, wespeaker_ready as model_ready, WESPEAKER_SHA256 as MODEL_SHA256, WESPEAKER_URL as MODEL_URL};

const SR: usize = 16000;
const MIN_LINE: f64 = 0.8; // seconds; shorter lines take the speaker of their neighbours
const MAX_LINE: f64 = 8.0; // seconds of each line used for its fingerprint
const FN: usize = 400;
const FHOP: usize = 160;
const FFT_N: usize = 512;
const MELS: usize = 80;

fn sha256_hex(data: &[u8]) -> String {
    // small, dependency-free SHA-256 (the model is only 26 MB, checked once)
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), v[0], v[1], v[2], v[3].wrapping_add(t1), v[4], v[5], v[6]];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

pub fn download_model(progress: &dyn Fn(f64)) -> Result<()> {
    if model_ready() {
        return Ok(());
    }
    let dest = model_path();
    std::fs::create_dir_all(dest.parent().unwrap())?;
    let mut resp = ureq::get(MODEL_URL).header("User-Agent", "StreamTranslator").call()?;
    let size = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse::<f64>().ok()).unwrap_or(26_530_309.0);
    let mut body = resp.body_mut().as_reader();
    let mut data = vec![];
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let k = body.read(&mut buf)?;
        if k == 0 {
            break;
        }
        data.extend_from_slice(&buf[..k]);
        progress(data.len() as f64 / size);
    }
    if sha256_hex(&data) != MODEL_SHA256 {
        return job_err("The speaker model failed its checksum. Try again.");
    }
    let tmp = dest.with_extension("part");
    std::fs::File::create(&tmp)?.write_all(&data)?;
    std::fs::rename(tmp, dest)?;
    Ok(())
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn mel_banks() -> Vec<[f32; FFT_N / 2 + 1]> {
    let mel = |f: f64| 1127.0 * (1.0 + f / 700.0).ln();
    let (lo, hi) = (mel(20.0), mel(8000.0));
    let centers: Vec<f64> = (0..MELS + 2).map(|i| lo + (hi - lo) * i as f64 / (MELS + 1) as f64).collect();
    (0..MELS)
        .map(|m| {
            let (l, c, r) = (centers[m], centers[m + 1], centers[m + 2]);
            let mut b = [0f32; FFT_N / 2 + 1];
            for (k, v) in b.iter_mut().enumerate() {
                let fm = mel(k as f64 * SR as f64 / FFT_N as f64);
                *v = ((fm - l) / (c - l)).min((r - fm) / (r - c)).max(0.0) as f32;
            }
            b
        })
        .collect()
}

/// 80-dim Kaldi-style log-mel features (25 ms / 10 ms, povey window), mean-normalised.
fn fbank(x: &[f32]) -> (usize, Vec<f32>) {
    let banks = mel_banks();
    let mut x: Vec<f32> = x.iter().map(|v| v * 32768.0).collect();
    if x.len() < FN {
        x.resize(FN, 0.0);
    }
    let n = 1 + (x.len() - FN) / FHOP;
    let win: Vec<f32> = (0..FN).map(|i| (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FN - 1) as f32).cos()).powf(0.85)).collect();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_N);
    let mut buf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let mut feats = vec![0f32; n * MELS];
    for t in 0..n {
        let fr = &x[t * FHOP..t * FHOP + FN];
        let mean = fr.iter().sum::<f32>() / FN as f32;
        buf.iter_mut().for_each(|v| *v = 0.0);
        let mut prev = fr[0] - mean;
        buf[0] = prev * (1.0 - 0.97) * win[0];
        for i in 1..FN {
            let cur = fr[i] - mean;
            buf[i] = (cur - 0.97 * prev) * win[i];
            prev = cur;
        }
        fft.process(&mut buf, &mut spec).unwrap();
        for m in 0..MELS {
            let e: f32 = spec.iter().zip(banks[m].iter()).map(|(s, b)| s.norm_sqr() * b).sum();
            feats[t * MELS + m] = e.max(f32::EPSILON).ln();
        }
    }
    for m in 0..MELS {
        let mean = (0..n).map(|t| feats[t * MELS + m]).sum::<f32>() / n as f32;
        (0..n).for_each(|t| feats[t * MELS + m] -= mean);
    }
    (n, feats)
}

fn embed(sess: &mut Session, x: &[f32]) -> Result<Vec<f32>> {
    let (n, f) = fbank(x);
    let out = sess.run(ort::inputs!["feats" => Tensor::from_array(([1usize, n, MELS], f))?])?;
    let (_, e) = out["embs"].try_extract_tensor::<f32>()?;
    let norm = e.iter().map(|v| v * v).sum::<f32>().sqrt() + 1e-9;
    Ok(e.iter().map(|v| v / norm).collect())
}

/// Speaker number (0 = talks most) for each (start, end) span in seconds of `audio`.
pub fn label(audio: &[f32], spans: &[(f64, f64)], progress: &dyn Fn(f64), cancel: &Cancel) -> Result<Vec<usize>> {
    if spans.is_empty() {
        return Ok(vec![]);
    }
    let mut guard = SESSION.lock().unwrap();
    if guard.is_none() {
        let env = gpu::ort_env()?;
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).max(4) / 2;
        let open = || -> ort::Result<Session> { Session::builder(env)?.with_intra_threads(threads)?.commit_from_file(model_path()) };
        *guard = Some(open()?);
    }
    let sess = guard.as_mut().unwrap();
    let (mut keep, mut embs, mut weights) = (vec![], vec![], vec![]);
    for (i, &(s, e)) in spans.iter().enumerate() {
        cancel.check()?;
        if e - s >= MIN_LINE {
            let mid = (s + e) / 2.0;
            let half = (e - s).min(MAX_LINE) / 2.0;
            let a = (((mid - half) * SR as f64) as usize).min(audio.len());
            let b = (((mid + half) * SR as f64) as usize).min(audio.len());
            if b - a >= (MIN_LINE * SR as f64) as usize {
                keep.push(i);
                embs.push(embed(sess, &audio[a..b])?);
                weights.push(e - s);
            }
        }
        progress((i + 1) as f64 / spans.len() as f64);
    }
    let mut out: Vec<Option<usize>> = vec![None; spans.len()];
    if !keep.is_empty() {
        let labels = cluster::cluster(&embs, &weights, cluster::SAME);
        let ranks = cluster::rank_by_talk_time(&labels, &weights);
        for (i, r) in keep.iter().zip(ranks) {
            out[*i] = Some(r);
        }
    }
    // short lines: same speaker as the closest line in time that has one
    let known: Vec<usize> = (0..out.len()).filter(|&i| out[i].is_some()).collect();
    Ok((0..out.len())
        .map(|i| {
            out[i].unwrap_or_else(|| {
                known.iter().min_by_key(|&&k| (k as i64 - i as i64).abs()).map(|&k| out[k].unwrap()).unwrap_or(0)
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn sha256_known_value() {
        assert_eq!(super::sha256_hex(b"hello"), "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
    }
}
