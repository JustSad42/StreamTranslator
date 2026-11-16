//! Sound from this PC's audio devices, as 16 kHz mono (what Whisper and voice detection want).
//!
//! Microphones and other recording devices are captured directly. Playback devices (a headset, speakers,
//! a virtual cable's output) are captured with WASAPI loopback: what plays on them is recorded.

use anyhow::{anyhow, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, StreamConfig,
};
use serde_json::{json, Value};

const SR: f64 = 16000.0;

/// Every recording and playback device: [{id, name, kind: "input"|"output", default}].
pub fn list() -> Value {
    let host = cpal::default_host();
    let def_in = host.default_input_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let def_out = host.default_output_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let mut out = vec![];
    for (kind, devs, def) in [("input", host.input_devices(), &def_in), ("output", host.output_devices(), &def_out)] {
        let Ok(devs) = devs else { continue };
        for d in devs {
            let Ok(id) = d.id().map(|i| i.to_string()) else { continue };
            let name = d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| d.to_string());
            out.push(json!({"id": id, "name": name, "kind": kind, "default": def.as_deref() == Some(id.as_str())}));
        }
    }
    json!(out)
}

/// A running capture; dropping it stops the device.
pub struct Capture {
    _stream: cpal::Stream,
    pub name: String,
}

/// Start capturing `id` (from `list`). `on_audio` gets 16 kHz mono samples, about every 10 ms.
/// Recording devices are recorded; playback devices are recorded in loopback.
pub fn capture(id: &str, mut on_audio: impl FnMut(&[f32]) + Send + 'static, on_error: impl Fn(String) + Send + 'static) -> Result<Capture> {
    let host = cpal::default_host();
    let dev_id = id.parse().map_err(|e| anyhow!("Unknown audio device {id}: {e}"))?;
    let dev = host.device_by_id(&dev_id).ok_or_else(|| anyhow!("That audio device isn't connected any more. Pick another one."))?;
    let name = dev.description().map(|x| x.name().to_string()).unwrap_or_else(|_| dev.to_string());
    let input = dev.supports_input();
    let cfg = if input { dev.default_input_config()? } else { dev.default_output_config()? };
    let (format, channels, rate) = (cfg.sample_format(), cfg.channels() as usize, cfg.sample_rate() as f64);
    let config: StreamConfig = cfg.into();
    let mut rs = Resampler::new(rate);
    let mut mono: Vec<f32> = vec![];
    let mut out: Vec<f32> = vec![];
    let mut feed = move |frames: &mut dyn Iterator<Item = f32>| {
        mono.clear();
        let mut acc = 0.0f32;
        for (i, v) in frames.enumerate() {
            acc += v;
            if (i + 1) % channels == 0 {
                mono.push(acc / channels as f32);
                acc = 0.0;
            }
        }
        out.clear();
        rs.process(&mono, &mut out);
        if !out.is_empty() {
            on_audio(&out);
        }
    };
    // Glitches (a buffer under/overrun when the PC is busy, a refused real-time thread, an automatic
    // reroute) lose at most a few milliseconds and the stream keeps running, so they aren't errors.
    let err = move |e: cpal::Error| {
        use cpal::ErrorKind as K;
        if !matches!(e.kind(), K::Xrun | K::RealtimeDenied | K::DeviceChanged) {
            on_error(e.to_string());
        }
    };
    let stream = match format {
        SampleFormat::F32 => dev.build_input_stream(config, move |d: &[f32], _: &_| feed(&mut d.iter().copied()), err, None)?,
        SampleFormat::I16 => dev.build_input_stream(config, move |d: &[i16], _: &_| feed(&mut d.iter().map(|v| *v as f32 / 32768.0)), err, None)?,
        SampleFormat::I32 => dev.build_input_stream(config, move |d: &[i32], _: &_| feed(&mut d.iter().map(|v| *v as f32 / 2147483648.0)), err, None)?,
        SampleFormat::U16 => dev.build_input_stream(config, move |d: &[u16], _: &_| feed(&mut d.iter().map(|v| (*v as f32 - 32768.0) / 32768.0)), err, None)?,
        f => return Err(anyhow!("{name} uses an audio format the app can't read ({f:?}).")),
    };
    stream.play()?;
    Ok(Capture { _stream: stream, name })
}

/// Any rate to 16 kHz: a windowed-sinc low-pass at 7.2 kHz, then linear interpolation between the
/// filtered samples. Keeps its state across calls, so chunks join seamlessly.
pub struct Resampler {
    step: f64,
    taps: Vec<f32>,
    hist: Vec<f32>,
    filtered: Vec<f32>,
    pos: f64,
}

impl Resampler {
    pub fn new(rate: f64) -> Resampler {
        let step = rate / SR;
        // Blackman window: ~1.2 kHz transition band (7.2 kHz passes, 8 kHz and up are removed)
        let n = ((5.5 * rate / 1200.0) as usize) | 1;
        let fc = 7200.0 / rate;
        let mid = (n / 2) as f64;
        let mut taps: Vec<f32> = (0..n)
            .map(|i| {
                let x = i as f64 - mid;
                let sinc = if x == 0.0 { 2.0 * fc } else { (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x) };
                let w = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos() + 0.08 * (4.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
                (sinc * w) as f32
            })
            .collect();
        let sum: f32 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        Resampler { step, hist: vec![0.0; n - 1], taps, filtered: vec![], pos: 0.0 }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if (self.step - 1.0).abs() < 1e-9 {
            out.extend_from_slice(input);
            return;
        }
        let n = self.taps.len();
        self.hist.extend_from_slice(input);
        let ready = self.hist.len() + 1 - n;
        for i in 0..ready {
            let w = &self.hist[i..i + n];
            self.filtered.push(w.iter().zip(&self.taps).map(|(a, b)| a * b).sum());
        }
        self.hist.drain(..ready);
        while self.pos + 1.0 < self.filtered.len() as f64 {
            let i = self.pos as usize;
            let f = (self.pos - i as f64) as f32;
            out.push(self.filtered[i] * (1.0 - f) + self.filtered[i + 1] * f);
            self.pos += self.step;
        }
        let used = (self.pos as usize).min(self.filtered.len());
        self.filtered.drain(..used);
        self.pos -= used as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::Resampler;

    #[test]
    fn resampler_keeps_a_tone_and_the_length() {
        // 1 s of 440 Hz at 48 kHz, fed in odd-sized chunks
        let src: Vec<f32> = (0..48000).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 48000.0).sin()).collect();
        let mut rs = Resampler::new(48000.0);
        let mut out = vec![];
        for c in src.chunks(441) {
            rs.process(c, &mut out);
        }
        assert!((out.len() as i64 - 16000).abs() < 40, "{}", out.len());
        // compare with the ideal 16 kHz tone (the filter delays by half its length)
        let delay = (rs.taps.len() / 2) as f32 / 48000.0;
        let err: f32 = out[1000..15000]
            .iter()
            .enumerate()
            .map(|(k, v)| (v - (2.0 * std::f32::consts::PI * 440.0 * ((k + 1000) as f32 / 16000.0 - delay)).sin()).abs())
            .fold(0.0, f32::max);
        assert!(err < 0.02, "max error {err}");
        // 7.9 kHz (above 16 kHz's limit) is removed, not folded back in
        let hi: Vec<f32> = (0..48000).map(|i| (2.0 * std::f32::consts::PI * 7900.0 * i as f32 / 48000.0).sin()).collect();
        let mut rs = Resampler::new(48000.0);
        let mut o2 = vec![];
        rs.process(&hi, &mut o2);
        let rms = (o2[500..].iter().map(|v| v * v).sum::<f32>() / (o2.len() - 500) as f32).sqrt();
        assert!(rms < 0.1, "rms {rms}");
    }
}
