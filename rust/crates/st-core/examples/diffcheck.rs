//! Differential check against the Python app: same inputs through both glossaries and SRT writers.
//! Usage: diffcheck <dir with diff_in.json and diff_py.srt>
use serde_json::Value;
use st_core::{glossary::Glossary, paths, subs::{write_srt, Segment}};

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let d: Value = serde_json::from_slice(&std::fs::read(dir.join("diff_in.json")).unwrap()).unwrap();
    let gl = Glossary::load(&paths::data_dir());
    let strs = |v: &Value| -> Vec<String> { v.as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect() };
    let players = strs(&d["inputs"]["players"]);
    let mut bad = 0;
    let mut check = |name: &str, got: Vec<(String, usize)>, want: &Value, inputs: &[String]| {
        let want: Vec<(String, usize)> = want.as_array().unwrap().iter().map(|w| (w[0].as_str().unwrap().to_string(), w[1].as_u64().unwrap() as usize)).collect();
        let same = got.iter().zip(&want).filter(|(a, b)| a == b).count();
        println!("{name}: {same}/{} identical", want.len());
        for ((g, w), i) in got.iter().zip(&want).zip(inputs).filter(|((a, b), _)| a != b).take(5) {
            bad += 1;
            println!("  in:     {i}\n  rust:   {g:?}\n  python: {w:?}");
        }
    };
    let es = strs(&d["inputs"]["es"]);
    let en = strs(&d["inputs"]["en"]);
    check("prepare (es slang)", es.iter().map(|t| gl.prepare(t, Some("es"))).collect(), &d["prepare_es"], &es);
    check("fix (english)", en.iter().map(|t| gl.fix(t, true, &players)).collect(), &d["fix_en"], &en);
    check("fix (original)", es.iter().map(|t| gl.fix(t, false, &players)).collect(), &d["fix_orig"], &es);
    let hot: Vec<Option<String>> = [true, false].iter().map(|&c| gl.hotwords(&["Orianna".into(), "K'Sante".into()], c, &players)).collect();
    let want_hot: Vec<Option<String>> = serde_json::from_value(d["hot"].clone()).unwrap();
    println!("hotwords: {}", if hot == want_hot { "identical" } else { "DIFFER" });
    let segs: Vec<Segment> = serde_json::from_value(d["segs"].clone()).unwrap();
    write_srt(&segs, &dir.join("diff_rs.srt"), 1.24).unwrap();
    let same = std::fs::read(dir.join("diff_rs.srt")).unwrap() == std::fs::read(dir.join("diff_py.srt")).unwrap();
    println!("srt from the same 78 segments (speaker colours, 1.24 s shift): {}", if same { "byte-identical" } else { "DIFFERS" });
    std::process::exit(if bad == 0 && same && hot == want_hot { 0 } else { 1 });
}
