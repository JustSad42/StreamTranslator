//! Whisper's log-mel features, matching faster-whisper's FeatureExtractor (checked to 1e-5).

use realfft::RealFftPlanner;

pub const SR: usize = 16000;
const N_FFT: usize = 400;
pub const HOP: usize = 160;
pub const N_FRAMES: usize = 3000; // one 30 s window

fn mel_filters(n_mels: usize) -> Vec<Vec<f64>> {
    let fftfreqs: Vec<f64> = (0..=N_FFT / 2).map(|i| i as f64 * SR as f64 / N_FFT as f64).collect();
    let max_mel = 45.245640471924965;
    let f_sp = 200.0 / 3.0;
    let (min_log_hz, min_log_mel, logstep) = (1000.0, 1000.0 / f_sp, 6.4f64.ln() / 27.0);
    let freqs: Vec<f64> = (0..n_mels + 2)
        .map(|i| {
            let m = max_mel * i as f64 / (n_mels + 1) as f64;
            if m >= min_log_mel {
                min_log_hz * (logstep * (m - min_log_mel)).exp()
            } else {
                f_sp * m
            }
        })
        .collect();
    (0..n_mels)
        .map(|m| {
            let enorm = 2.0 / (freqs[m + 2] - freqs[m]);
            fftfreqs
                .iter()
                .map(|&f| {
                    let lower = -(freqs[m] - f) / (freqs[m + 1] - freqs[m]);
                    let upper = (freqs[m + 2] - f) / (freqs[m + 2] - freqs[m + 1]);
                    lower.min(upper).max(0.0) * enorm
                })
                .collect()
        })
        .collect()
}

/// Features as [n_mels][frames] (row-major). Frames = content frames (the last STFT frame is dropped).
pub struct Mel {
    pub n_mels: usize,
    pub frames: usize,
    pub data: Vec<f32>,
}

impl Mel {
    pub fn new(audio: &[f32], n_mels: usize) -> Mel {
        let filters = mel_filters(n_mels);
        let mut x = audio.to_vec();
        x.resize(audio.len() + 160, 0.0);
        let half = N_FFT / 2;
        let n = x.len();
        let get = |j: isize| -> f32 {
            if n == 1 {
                return x[0];
            }
            let j = if j < 0 { -j } else if j as usize >= n { 2 * (n as isize - 1) - j } else { j };
            x[j.clamp(0, n as isize - 1) as usize]
        };
        let total = 1 + (n + 2 * half - N_FFT) / HOP;
        let frames = total - 1;
        let win: Vec<f32> = (0..N_FFT).map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N_FFT as f64).cos()) as f32).collect();
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(N_FFT);
        let mut buf = fft.make_input_vec();
        let mut spec = fft.make_output_vec();
        let mut data = vec![0f32; n_mels * frames];
        let mut mag = vec![0f32; N_FFT / 2 + 1];
        for t in 0..frames {
            for i in 0..N_FFT {
                buf[i] = get((t * HOP + i) as isize - half as isize) * win[i];
            }
            fft.process(&mut buf, &mut spec).unwrap();
            for (k, c) in spec.iter().enumerate() {
                mag[k] = c.norm_sqr();
            }
            for m in 0..n_mels {
                let v: f64 = filters[m].iter().zip(&mag).map(|(a, b)| a * *b as f64).sum();
                data[m * frames + t] = (v as f32).max(1e-10).log10();
            }
        }
        let mx = data.iter().cloned().fold(f32::MIN, f32::max);
        for v in data.iter_mut() {
            *v = (v.max(mx - 8.0) + 4.0) / 4.0;
        }
        Mel { n_mels, frames, data }
    }

    /// A 30 s window starting at `seek` frames, zero-padded (pad_or_trim).
    pub fn window(&self, seek: usize, size: usize) -> Vec<f32> {
        let mut w = vec![0f32; self.n_mels * N_FRAMES];
        let size = size.min(N_FRAMES).min(self.frames.saturating_sub(seek));
        for m in 0..self.n_mels {
            w[m * N_FRAMES..m * N_FRAMES + size].copy_from_slice(&self.data[m * self.frames + seek..m * self.frames + seek + size]);
        }
        w
    }
}
