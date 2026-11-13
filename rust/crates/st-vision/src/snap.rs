//! Snap a section to the exact League game using the in-game clock (snap.find_game).
//!
//! Any "mm:ss" read at VOD time T votes for "the game started at T - mm:ss". Only the in-game clock's
//! votes agree across frames, so the biggest agreeing cluster is the game start. The end is found by
//! walking forward until that clock disappears, then narrowing it down.

use anyhow::Result;
use serde_json::{json, Value};
use st_core::{
    cancel::Cancel,
    job_err,
    ocrtext::{clock_values, minute_variants},
    vod::VodInfo,
};

/// seconds two votes may differ and still agree
const TOL: f64 = 3.0;

fn readings(info: &VodInfo, t: f64) -> Option<Vec<u32>> {
    let png = st_media::ffmpeg::top_strip(info, t).ok()?;
    Some(clock_values(&crate::ocr::ocr(&png).ok()?))
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// {game_start, start, end, readings, clock_at_lo}
pub fn find_game(info: &VodInfo, lo: f64, hi: f64, progress: &(dyn Fn(f64, &str) + Sync), cancel: &Cancel) -> Result<Value> {
    let dur = if info.duration > 0.0 { info.duration } else { hi };
    let span = (hi - lo).max(60.0);
    let n = 12;
    let times: Vec<f64> = (0..n).map(|i| lo + span * (i as f64 + 0.5) / n as f64).collect();
    progress(0.02, "Reading the game clock…");

    // (sample time, implied game start), 4 frames at a time
    let mut votes: Vec<(f64, f64)> = vec![];
    for (k, group) in times.chunks(4).enumerate() {
        cancel.check()?;
        let found: Vec<(f64, Vec<u32>)> = std::thread::scope(|s| {
            let hs: Vec<_> = group.iter().map(|&t| s.spawn(move || (t, readings(info, t).unwrap_or_default()))).collect();
            hs.into_iter().filter_map(|h| h.join().ok()).collect()
        });
        for (t, rs) in found {
            votes.extend(rs.iter().map(|&g| (t, t - g as f64)).filter(|(_, v)| (-60.0..=dur).contains(v)));
        }
        progress(0.05 + 0.45 * ((k + 1) * 4).min(n) as f64 / n as f64, "Reading the game clock…");
    }
    if votes.is_empty() {
        return job_err("Couldn't see a League game clock in that section. Make sure the section covers the game, with the in-game HUD visible.");
    }
    // biggest group of agreeing votes, counting each sample time once
    let distinct = |g: &[(f64, f64)]| {
        let mut ts: Vec<u64> = g.iter().map(|(t, _)| t.to_bits()).collect();
        ts.sort();
        ts.dedup();
        ts.len()
    };
    let mut best: Vec<(f64, f64)> = vec![];
    for &(_, v) in &votes {
        let group: Vec<(f64, f64)> = votes.iter().cloned().filter(|(_, x)| (x - v).abs() <= TOL).collect();
        if distinct(&group) > distinct(&best) {
            best = group;
        }
    }
    let mut samples: Vec<f64> = best.iter().map(|(t, _)| *t).collect();
    samples.sort_by(|a, b| a.total_cmp(b));
    samples.dedup();
    if samples.len() < 2 {
        return job_err("Found numbers on screen but no game clock that keeps time. Try a section that's mostly the game itself.");
    }
    let start = median(best.iter().map(|(_, x)| *x).collect());
    let is_game = |t: f64| readings(info, t).is_some_and(|rs| rs.iter().any(|&g| minute_variants(g).iter().any(|&v| ((t - v as f64) - start).abs() <= TOL)));

    // walk forward from the last frame that showed the clock
    let mut last_ok = *samples.last().unwrap();
    let mut t = last_ok;
    let (mut misses, mut first_bad) = (0, None::<f64>);
    let limit = dur.min(hi.max(last_ok) + 25.0 * 60.0);
    progress(0.55, "Finding the end of the game…");
    while t + 30.0 <= limit {
        cancel.check()?;
        t += 30.0;
        if is_game(t) {
            (last_ok, misses, first_bad) = (t, 0, None);
        } else {
            misses += 1;
            first_bad = first_bad.or(Some(t));
            if misses >= 3 {
                // 90 s without the clock: replays or a full-screen camera can hide it for a while, so look
                // a few minutes further before calling it.
                let mut back = None;
                let mut t2 = (t as i64 + 60) as f64;
                while t2 <= limit.min(t + 300.0) {
                    if cancel.is_set() {
                        break;
                    }
                    if is_game(t2) {
                        back = Some(t2);
                        break;
                    }
                    t2 += 60.0;
                }
                match back {
                    None => break,
                    Some(b) => (t, last_ok, misses, first_bad) = (b, b, 0, None),
                }
            }
        }
        let s_last = *samples.last().unwrap();
        progress((0.55 + 0.3 * (t - s_last) / (limit - s_last).max(1.0)).min(0.85), "Finding the end of the game…");
    }
    let end = match first_bad {
        None => dur.min(last_ok + 10.0),
        Some(fb) => {
            let (mut a, mut b) = (last_ok, fb);
            while b - a > 3.0 {
                cancel.check()?;
                let mid = (a + b) / 2.0;
                if is_game(mid) {
                    a = mid;
                } else {
                    b = mid;
                }
                progress(0.9, "Pinning down the last second…");
            }
            dur.min(a + 10.0) // a few seconds for the nexus explosion / victory screen
        }
    };
    progress(1.0, "Done");
    Ok(json!({"game_start": start, "start": (start - 3.0).max(0.0), "end": end, "readings": samples.len(), "clock_at_lo": lo - start}))
}
