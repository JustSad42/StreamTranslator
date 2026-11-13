//! Champion portraits matched against Riot's icons (detect.champions_in). A re-implementation of OpenCV's
//! matchTemplate(TM_CCOEFF_NORMED) with FFTs; scores matched OpenCV's to 2e-5 and INTER_AREA resizing is
//! pixel-identical, so the tuned 0.80 threshold still applies.

use std::{collections::HashMap, path::Path, sync::Arc};

use rayon::prelude::*;
use rustfft::{num_complex::Complex32, Fft, FftPlanner};

pub const SIZES: [usize; 7] = [18, 22, 26, 31, 37, 44, 52];
pub const THRESHOLD: f32 = 0.80;
pub const SHORTLIST: usize = 30;

/// 8-bit 3-channel image, row-major, interleaved.
#[derive(Clone)]
pub struct Img {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Img {
    pub fn from_png(bytes: &[u8]) -> anyhow::Result<Img> {
        let i = image::load_from_memory(bytes)?.to_rgb8();
        Ok(Img { w: i.width() as usize, h: i.height() as usize, px: i.into_raw() })
    }
    pub fn crop(&self, x: usize, y: usize, w: usize, h: usize) -> Img {
        let mut px = Vec::with_capacity(w * h * 3);
        for r in y..y + h {
            px.extend_from_slice(&self.px[(r * self.w + x) * 3..(r * self.w + x + w) * 3]);
        }
        Img { w, h, px }
    }
    /// OpenCV INTER_AREA for downscaling: each output pixel is the area-weighted mean of the input.
    pub fn resize_area(&self, nw: usize, nh: usize) -> Img {
        let weights = |n: usize, nn: usize| -> Vec<Vec<(usize, f64)>> {
            let s = n as f64 / nn as f64;
            (0..nn)
                .map(|o| {
                    let (a, b) = (o as f64 * s, (o + 1) as f64 * s);
                    let mut v = vec![];
                    let mut i = a.floor() as usize;
                    while (i as f64) < b && i < n {
                        let w = (b.min(i as f64 + 1.0) - a.max(i as f64)) / s;
                        if w > 1e-9 {
                            v.push((i, w));
                        }
                        i += 1;
                    }
                    v
                })
                .collect()
        };
        let (wx, wy) = (weights(self.w, nw), weights(self.h, nh));
        let mut px = vec![0u8; nw * nh * 3];
        for (oy, ry) in wy.iter().enumerate() {
            for (ox, rx) in wx.iter().enumerate() {
                for c in 0..3 {
                    let mut acc = 0f64;
                    for &(iy, a) in ry {
                        for &(ix, b) in rx {
                            acc += a * b * self.px[(iy * self.w + ix) * 3 + c] as f64;
                        }
                    }
                    px[(oy * nw + ox) * 3 + c] = acc.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        Img { w: nw, h: nh, px }
    }
}

/// 2D FFT helper for one fixed frame size.
pub struct Fft2 {
    w: usize,
    h: usize,
    row_f: Arc<dyn Fft<f32>>,
    col_f: Arc<dyn Fft<f32>>,
    row_i: Arc<dyn Fft<f32>>,
    col_i: Arc<dyn Fft<f32>>,
}

impl Fft2 {
    pub fn new(w: usize, h: usize) -> Self {
        let mut p = FftPlanner::new();
        Fft2 { w, h, row_f: p.plan_fft_forward(w), col_f: p.plan_fft_forward(h), row_i: p.plan_fft_inverse(w), col_i: p.plan_fft_inverse(h) }
    }
    /// in place, data is h rows of w; `rows` = only the first `rows` rows can be non-zero (skips the rest)
    fn run(&self, d: &mut [Complex32], inverse: bool, rows: usize) {
        let (rf, cf) = if inverse { (&self.row_i, &self.col_i) } else { (&self.row_f, &self.col_f) };
        if inverse {
            // columns first then rows, so the "rows" shortcut is only used forward
            self.cols(d, cf);
            rf.process(d);
        } else {
            rf.process(&mut d[..rows * self.w]);
            self.cols(d, cf);
        }
    }
    fn cols(&self, d: &mut [Complex32], f: &Arc<dyn Fft<f32>>) {
        let (w, h) = (self.w, self.h);
        let mut t = vec![Complex32::default(); w * h];
        for y in 0..h {
            for x in 0..w {
                t[x * h + y] = d[y * w + x];
            }
        }
        f.process(&mut t);
        for y in 0..h {
            for x in 0..w {
                d[y * w + x] = t[x * h + y];
            }
        }
    }
}

/// A frame prepared for matching: per-channel FFT (mean removed) and integral images.
pub struct Prepared {
    w: usize,
    h: usize,
    spec: [Vec<Complex32>; 3],
    s1: [Vec<f64>; 3],
    s2: [Vec<f64>; 3],
}

pub fn prepare(img: &Img, fft: &Fft2) -> Prepared {
    let (w, h) = (img.w, img.h);
    let mk = |c: usize| {
        let mean = (0..w * h).map(|i| img.px[i * 3 + c] as f64).sum::<f64>() / (w * h) as f64;
        let mut d: Vec<Complex32> = (0..w * h).map(|i| Complex32::new((img.px[i * 3 + c] as f64 - mean) as f32, 0.0)).collect();
        fft.run(&mut d, false, h);
        let mut s1 = vec![0f64; (w + 1) * (h + 1)];
        let mut s2 = vec![0f64; (w + 1) * (h + 1)];
        for y in 0..h {
            let (mut r1, mut r2) = (0f64, 0f64);
            for x in 0..w {
                let v = img.px[(y * w + x) * 3 + c] as f64;
                r1 += v;
                r2 += v * v;
                s1[(y + 1) * (w + 1) + x + 1] = s1[y * (w + 1) + x + 1] + r1;
                s2[(y + 1) * (w + 1) + x + 1] = s2[y * (w + 1) + x + 1] + r2;
            }
        }
        (d, s1, s2)
    };
    let (a, b, c) = (mk(0), mk(1), mk(2));
    Prepared { w, h, spec: [a.0, b.0, c.0], s1: [a.1, b.1, c.1], s2: [a.2, b.2, c.2] }
}

/// Template FFT (zero-mean per channel, conjugated) + its norm, for one frame size.
pub struct PrepTpl {
    tw: usize,
    th: usize,
    spec: Vec<Complex32>, // 3 channels back to back
    norm2: f64,
}

pub fn prep_tpl(t: &Img, fft: &Fft2) -> PrepTpl {
    let (w, h) = (fft.w, fft.h);
    let n = (t.w * t.h) as f64;
    let mut spec = vec![Complex32::default(); 3 * w * h];
    let mut norm2 = 0f64;
    for c in 0..3 {
        let mean = (0..t.w * t.h).map(|i| t.px[i * 3 + c] as f64).sum::<f64>() / n;
        let d = &mut spec[c * w * h..(c + 1) * w * h];
        for y in 0..t.h {
            for x in 0..t.w {
                let v = t.px[(y * t.w + x) * 3 + c] as f64 - mean;
                norm2 += v * v;
                d[y * w + x] = Complex32::new(v as f32, 0.0);
            }
        }
        fft.run(d, false, t.h);
        d.iter_mut().for_each(|v| *v = v.conj());
    }
    PrepTpl { tw: t.w, th: t.h, spec, norm2 }
}

/// Best TM_CCOEFF_NORMED score of the template anywhere in the frame (optionally the whole map).
pub fn best_score(f: &Prepared, t: &PrepTpl, fft: &Fft2, map: Option<&mut Vec<f32>>) -> f32 {
    let (w, h) = (f.w, f.h);
    let mut acc = vec![Complex32::default(); w * h];
    for c in 0..3 {
        let ts = &t.spec[c * w * h..(c + 1) * w * h];
        for ((a, x), y) in acc.iter_mut().zip(&f.spec[c]).zip(ts) {
            *a += x * y;
        }
    }
    fft.run(&mut acc, true, h);
    let scale = 1.0 / (w * h) as f64;
    let n = (t.tw * t.th) as f64;
    let (mw, mh) = (w - t.tw + 1, h - t.th + 1);
    let mut best = f32::MIN;
    let mut out = map.map(|m| {
        m.clear();
        m.resize(mw * mh, 0.0);
        m
    });
    for y in 0..mh {
        for x in 0..mw {
            let num = acc[y * w + x].re as f64 * scale;
            let mut var = 0f64;
            for c in 0..3 {
                let s = |a: &Vec<f64>| a[(y + t.th) * (w + 1) + x + t.tw] - a[y * (w + 1) + x + t.tw] - a[(y + t.th) * (w + 1) + x] + a[y * (w + 1) + x];
                let (a, b) = (s(&f.s1[c]), s(&f.s2[c]));
                var += (b - a * a / n).max(0.0);
            }
            let den = (var * t.norm2).sqrt();
            // OpenCV's convention for the normalised methods
            let r = if num.abs() < den {
                num / den
            } else if num.abs() < den * 1.125 {
                num.signum()
            } else {
                0.0
            } as f32;
            if r > best {
                best = r;
            }
            if let Some(m) = out.as_deref_mut() {
                m[y * mw + x] = r;
            }
        }
    }
    best
}

pub fn load_icons(dir: &Path, champs: &[(String, String)]) -> Vec<(String, Vec<Img>)> {
    champs
        .par_iter()
        .filter_map(|(name, id)| {
            let img = Img::from_png(&std::fs::read(dir.join(format!("{id}.png"))).ok()?).ok()?;
            let m = (img.w.min(img.h) as f64 * 0.13) as usize;
            let core = img.crop(m, m, img.w - 2 * m, img.h - 2 * m);
            Some((name.clone(), SIZES.iter().map(|&s| core.resize_area(s, s)).collect()))
        })
        .collect()
}

pub fn champions_in(frames: &[Img], icons: &[(String, Vec<Img>)]) -> Vec<(String, f32)> {
    let (w, h) = (frames[0].w, frames[0].h);
    // quick pass at half size over every champion
    let small: Vec<Img> = frames.iter().map(|f| f.resize_area(f.w / 2, f.h / 2)).collect();
    let fs = Fft2::new(w / 2, h / 2);
    let sp: Vec<Prepared> = small.par_iter().map(|f| prepare(f, &fs)).collect();
    let coarse: Vec<(usize, f32)> = icons
        .par_iter()
        .enumerate()
        .map(|(k, (_, temps))| {
            let mut top = 0f32;
            for t in temps.iter().skip(1).step_by(2) {
                let s = (t.w / 2).max(8);
                let pt = prep_tpl(&t.resize_area(s, s), &fs);
                for f in &sp {
                    top = top.max(best_score(f, &pt, &fs, None));
                }
            }
            (k, top)
        })
        .collect();
    let mut order = coarse.clone();
    order.sort_by(|a, b| b.1.total_cmp(&a.1));
    let short: Vec<usize> = order.iter().take(SHORTLIST).map(|x| x.0).collect();
    // full-size pass on the shortlist
    let ff = Fft2::new(w, h);
    let fp: Vec<Prepared> = frames.par_iter().map(|f| prepare(f, &ff)).collect();
    let jobs: Vec<(usize, usize)> = short.iter().flat_map(|&k| (0..SIZES.len()).map(move |s| (k, s))).collect();
    let scores: Vec<(usize, f32)> = jobs
        .par_iter()
        .map(|&(k, s)| {
            let pt = prep_tpl(&icons[k].1[s], &ff);
            (k, fp.iter().map(|f| best_score(f, &pt, &ff, None)).fold(0f32, f32::max))
        })
        .collect();
    let mut best: HashMap<usize, f32> = HashMap::new();
    for (k, v) in scores {
        let e = best.entry(k).or_insert(0.0);
        *e = e.max(v);
    }
    let mut hits: Vec<(String, f32)> = best.into_iter().filter(|x| x.1 >= THRESHOLD).map(|(k, v)| (icons[k].0.clone(), v)).collect();
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    hits.truncate(10);
    hits
}

