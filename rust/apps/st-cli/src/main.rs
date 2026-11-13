//! Command-line version, same as `python -m streamtranslator.cli`:
//!     streamtranslator-cli <vod url> --start 1:02:30 --end 1:35:00

use std::{path::PathBuf, sync::Mutex};

use clap::Parser;
use st_core::{
    timefmt::{format_ts, parse_ts},
    Cancelled, JobError,
};
use st_job::{run_job, Callbacks, Engines, JobParams};

#[derive(Parser)]
#[command(name = "streamtranslator")]
struct Args {
    url: String,
    #[arg(long, value_parser = parse_ts)]
    start: f64,
    #[arg(long, value_parser = parse_ts)]
    end: f64,
    #[arg(long)]
    out: Option<PathBuf>,
    #[arg(long, default_value = "720p", value_parser = ["Source", "720p", "480p"])]
    quality: String,
    #[arg(long, default_value = "large-v3")]
    model: String,
    /// e.g. ko, es (default: auto-detect)
    #[arg(long)]
    language: Option<String>,
    /// also save original-language subtitles
    #[arg(long)]
    original: bool,
    #[arg(long)]
    no_burn: bool,
    #[arg(long, default_value = "Above game HUD", value_parser = ["Above game HUD", "Bottom", "Top"])]
    position: String,
    #[arg(long, default_value = "Medium", value_parser = ["Small", "Medium", "Large"])]
    size: String,
    // options the Python CLI doesn't expose, for testing the rest of the pipeline
    /// "two-step" or "whisper"
    #[arg(long, default_value = "two-step")]
    method: String,
    #[arg(long)]
    isolate: bool,
    #[arg(long)]
    speakers: bool,
}

fn main() {
    let a = Args::parse();
    let out = a.out.unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("Videos").join("StreamTranslator"));
    let last = Mutex::new(-1i64);
    let cb = Callbacks {
        stage: Box::new(|i, n, label| println!("[{i}/{n}] {label}")),
        progress: Box::new(move |f| {
            let pct = if f >= 0.0 { (f * 100.0) as i64 } else { -1 };
            let mut l = last.lock().unwrap();
            if pct != *l && pct % 10 == 0 {
                *l = pct;
                println!("    {pct}%");
            }
        }),
        log: Box::new(|s| println!("  {s}")),
        segment: Box::new(|seg, kind| println!("    {:>6} [{kind}] {}", format_ts(seg.start), seg.text)),
        ..Default::default()
    };
    let p = JobParams {
        url: a.url,
        start: a.start,
        end: a.end,
        out_dir: out,
        quality: a.quality,
        model: a.model,
        language: a.language,
        want_original: a.original,
        burn: !a.no_burn,
        position: a.position,
        font_size: a.size,
        method: a.method,
        isolate: a.isolate,
        speakers: a.speakers,
        ..Default::default()
    };
    let engines = Engines::new();
    match run_job(&p, &cb, &engines) {
        Ok(r) => {
            println!("\nDone in {:.0}s. Language: {} ({:.0}%), {} lines", r.seconds, r.language, r.probability * 100.0, r.segments);
            let name = |p: &PathBuf| p.file_name().unwrap().to_string_lossy().into_owned();
            println!("  Folder:     {}\n  Subtitles:  {}\n  Transcript: {}", r.folder.display(), name(&r.srt), name(&r.transcript));
            if let Some(s) = &r.subbed {
                println!("  Video:      {}", name(s));
            }
        }
        Err(e) => {
            let msg = if e.downcast_ref::<Cancelled>().is_some() || e.downcast_ref::<JobError>().is_some() { e.to_string() } else { format!("{e:#}") };
            eprintln!("Failed: {msg}");
            std::process::exit(1);
        }
    }
}
