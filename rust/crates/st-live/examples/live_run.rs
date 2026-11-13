//! Run a live session without the app and log its events as JSON lines (for comparing with Python).
//! live_run <channel or url> <seconds> <out.jsonl> <save dir>
use std::{io::Write, sync::{Arc, Mutex}, time::Instant};
use serde_json::{json, Value};
use st_live::{Emit, LiveOptions, LiveSession};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (src, secs, out, save) = (&a[1], a[2].parse::<f64>().unwrap(), &a[3], &a[4]);
    st_ml::gpu::setup(false);
    let file = Arc::new(Mutex::new(std::fs::File::create(out).unwrap()));
    let t0 = Instant::now();
    let done = st_core::cancel::Cancel::new();
    let (f2, d2) = (file.clone(), done.clone());
    let emit: Emit = Arc::new(move |kind: &str, data: Value| {
        if kind == "live.level" || kind == "live.frame" {
            return;
        }
        let mut m = json!({"type": kind, "at": (t0.elapsed().as_secs_f64() * 100.0).round() / 100.0});
        if let Value::Object(d) = data { m.as_object_mut().unwrap().extend(d); }
        if kind == "live.status" && m["state"] == "stopped" { d2.set(); }
        writeln!(f2.lock().unwrap(), "{m}").unwrap();
    });
    let gl = st_core::glossary::Glossary::load(&st_core::paths::data_dir());
    let opts = LiveOptions { source: src.clone(), model: "large-v3".into(), custom_model: None, language: None, show_original: true,
        use_glossary: true, champions: vec![], isolate: false, method: "two-step".into(), translator: "better".into(), save_dir: save.into() };
    let s = LiveSession::new(opts, Arc::new(st_job::Engines::new()), Arc::new(Mutex::new(gl)), emit);
    s.start();
    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
    s.stop();
    done.wait(30.0);
}
