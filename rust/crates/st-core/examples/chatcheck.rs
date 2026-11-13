//! Differential check of the chat filters against a JSON dump from the Python code (chat.py).
use st_core::chat::*;
fn main() {
    let path = std::env::args().nth(1).expect("python dump");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let (mut ok, mut bad) = (0, 0);
    for c in v.as_array().unwrap() {
        let m = c["m"].as_str().unwrap();
        let laughs: Vec<bool> = m.split_whitespace().map(is_laugh).collect();
        let core = parse_line(c["line"].as_str().unwrap(), 0.0).unwrap().core;
        let same = serde_json::json!(laughs) == c["laughs"] && strip_laughs(m) == c["strip"].as_str().unwrap()
            && is_english(m) == c["en"].as_bool().unwrap() && core.as_deref() == c["core"].as_str();
        if same { ok += 1 } else { bad += 1; if bad < 10 { println!("DIFF {m:?}: rust core {core:?} py {}", c["core"]) } }
    }
    println!("{ok} same, {bad} different");
}
