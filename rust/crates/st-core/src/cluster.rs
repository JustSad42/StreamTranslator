//! Grouping lines by voice (speakers.py: cluster, color).

pub const SAME: f64 = 0.3; // line-level cosine similarity for grouping (stream audio is noisy)
pub const MERGE: f64 = 0.3; // groups whose average voices are this close are the same person
pub const MINOR: f64 = 0.04; // a group with less of the talk time than this and <= 2 lines is folded in

/// Subtitle colours for speakers 2, 3, ... (speaker 1 stays white).
pub const COLORS: &[&str] = &["#FFD54F", "#4FC3F7", "#A5D6A7", "#F48FB1", "#CE93D8", "#FFAB91"];

pub fn color(n: usize) -> Option<&'static str> {
    (n > 0).then(|| COLORS[(n - 1) % COLORS.len()])
}

fn dot(a: &[f32], b: &[f32]) -> f64 {
    a.iter().zip(b).map(|(x, y)| *x as f64 * *y as f64).sum()
}

fn ids_of(labels: &[usize]) -> Vec<usize> {
    let mut ids = labels.to_vec();
    ids.sort();
    ids.dedup();
    ids
}

/// Average-linkage grouping by cosine similarity of unit-length embeddings. Returns a group per row.
pub fn cluster(embs: &[Vec<f32>], weights: &[f64], same: f64) -> Vec<usize> {
    let n = embs.len();
    if n <= 1 {
        return vec![0; n];
    }
    let mut sim = vec![vec![0f64; n]; n];
    for i in 0..n {
        for j in 0..n {
            sim[i][j] = if i == j { f64::NEG_INFINITY } else { dot(&embs[i], &embs[j]) };
        }
    }
    let mut size = vec![1f64; n];
    let mut alive = vec![true; n];
    let mut labels: Vec<usize> = (0..n).collect();
    while alive.iter().filter(|&&a| a).count() > 1 {
        let (mut a, mut b, mut best) = (0, 0, f64::NEG_INFINITY);
        for i in 0..n {
            for j in 0..n {
                if sim[i][j] > best {
                    (a, b, best) = (i, j, sim[i][j]);
                }
            }
        }
        if best < same {
            break;
        }
        let merged: Vec<f64> = (0..n).map(|k| (size[a] * sim[a][k] + size[b] * sim[b][k]) / (size[a] + size[b])).collect();
        for k in 0..n {
            sim[a][k] = merged[k];
            sim[k][a] = merged[k];
        }
        sim[a][a] = f64::NEG_INFINITY;
        for k in 0..n {
            sim[b][k] = f64::NEG_INFINITY;
            sim[k][b] = f64::NEG_INFINITY;
        }
        size[a] += size[b];
        alive[b] = false;
        for l in labels.iter_mut() {
            if *l == b {
                *l = a;
            }
        }
    }
    let dim = embs[0].len();
    // Single lines are noisy, so one person often ends up in several groups; merge groups whose
    // duration-weighted average voices are close.
    loop {
        let ids = ids_of(&labels);
        if ids.len() <= 1 {
            break;
        }
        let cent: Vec<Vec<f32>> = ids
            .iter()
            .map(|&g| {
                let mut c = vec![0f64; dim];
                for (i, e) in embs.iter().enumerate() {
                    if labels[i] == g {
                        for d in 0..dim {
                            c[d] += e[d] as f64 * weights[i];
                        }
                    }
                }
                let norm = c.iter().map(|v| v * v).sum::<f64>().sqrt() + 1e-9;
                c.iter().map(|v| (v / norm) as f32).collect()
            })
            .collect();
        let (mut a, mut b, mut best) = (0, 0, f64::NEG_INFINITY);
        for i in 0..ids.len() {
            for j in 0..ids.len() {
                let s = if i == j { -1.0 } else { dot(&cent[i], &cent[j]) };
                if s > best {
                    (a, b, best) = (i, j, s);
                }
            }
        }
        if best < MERGE {
            break;
        }
        let (ga, gb) = (ids[a], ids[b]);
        labels.iter_mut().filter(|l| **l == gb).for_each(|l| *l = ga);
    }
    // fold tiny groups (a one-off shout, a champion voice line) into the closest real one
    let total = {
        let t: f64 = weights.iter().sum();
        if t == 0.0 { 1.0 } else { t }
    };
    loop {
        let ids = ids_of(&labels);
        if ids.len() <= 1 {
            break;
        }
        let share = |g: usize| labels.iter().zip(weights).filter(|(l, _)| **l == g).map(|(_, w)| w).sum::<f64>() / total;
        let count = |g: usize| labels.iter().filter(|l| **l == g).count();
        let small: Vec<usize> = ids.iter().cloned().filter(|&g| share(g) < MINOR && count(g) <= 2).collect();
        if small.is_empty() {
            break;
        }
        let mut g = small[0];
        for &s in &small {
            if share(s) < share(g) {
                g = s;
            }
        }
        let mean = |k: usize| -> Vec<f32> {
            let rows: Vec<&Vec<f32>> = embs.iter().zip(&labels).filter(|(_, l)| **l == k).map(|(e, _)| e).collect();
            (0..dim).map(|d| (rows.iter().map(|r| r[d] as f64).sum::<f64>() / rows.len() as f64) as f32).collect()
        };
        let cg = mean(g);
        let mut near = None;
        let mut best = f64::NEG_INFINITY;
        for &k in ids.iter().filter(|&&k| k != g) {
            let s = dot(&cg, &mean(k));
            if s > best {
                best = s;
                near = Some(k);
            }
        }
        let near = near.unwrap();
        labels.iter_mut().filter(|l| **l == g).for_each(|l| *l = near);
    }
    labels
}

/// Speaker number per line: 0 = the most talk time. `labels` from `cluster`, `weights` = durations.
pub fn rank_by_talk_time(labels: &[usize], weights: &[f64]) -> Vec<usize> {
    let ids = ids_of(labels);
    let talk = |g: usize| labels.iter().zip(weights).filter(|(l, _)| **l == g).map(|(_, w)| w).sum::<f64>();
    let mut order = ids.clone();
    order.sort_by(|a, b| talk(*b).total_cmp(&talk(*a)));
    labels.iter().map(|l| order.iter().position(|g| g == l).unwrap()).collect()
}
