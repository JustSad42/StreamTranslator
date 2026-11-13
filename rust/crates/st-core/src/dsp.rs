//! Signal helpers.

use realfft::RealFftPlanner;

/// Seconds `d` such that `other[t + d]` matches `ref_[t]` (both 16 kHz mono), searched within ±`max_s`
/// using FFT cross-correlation of the first minute (pipeline.audio_offset).
pub fn audio_offset(ref_: &[f32], other: &[f32], max_s: f64) -> f64 {
    let n = ref_.len().min(other.len()).min(16000 * 60);
    if n < 16000 * 3 {
        return 0.0;
    }
    let centred = |x: &[f32]| -> Vec<f64> {
        let mean = x[..n].iter().map(|v| *v as f64).sum::<f64>() / n as f64;
        x[..n].iter().map(|v| *v as f64 - mean).collect()
    };
    let (a, b) = (centred(ref_), centred(other));
    let size = 1usize << (usize::BITS - (2 * n - 1).leading_zeros());
    let mut pl = RealFftPlanner::<f64>::new();
    let (fwd, inv) = (pl.plan_fft_forward(size), pl.plan_fft_inverse(size));
    let spec = |x: &[f64]| {
        let mut buf = vec![0f64; size];
        buf[..n].copy_from_slice(x);
        let mut out = fwd.make_output_vec();
        fwd.process(&mut buf, &mut out).unwrap();
        out
    };
    let (fa, fb) = (spec(&a), spec(&b));
    let mut prod: Vec<_> = fb.iter().zip(&fa).map(|(x, y)| x * y.conj()).collect();
    prod[0].im = 0.0;
    if let Some(l) = prod.last_mut() {
        l.im = 0.0;
    }
    let mut c = vec![0f64; size];
    inv.process(&mut prod, &mut c).unwrap();
    let m = (max_s * 16000.0) as usize;
    let cand: Vec<f64> = c[..m].iter().chain(&c[size - m..]).cloned().collect();
    let mut k = 0;
    for (i, v) in cand.iter().enumerate() {
        if *v > cand[k] {
            k = i;
        }
    }
    (if k < m { k as f64 } else { k as f64 - 2.0 * m as f64 }) / 16000.0
}
