//! Writes the isolated 16 kHz voice for a clip and its VAD chunks (for comparing with Python).
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let c = st_core::cancel::Cancel::new();
    let v = st_ml::isolate::voice_16k_from_file(std::path::Path::new(&a[1]), &|_| {}, &c).unwrap();
    std::fs::write(&a[2], v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
    let ch = st_ml::vad::speech_timestamps(&v, &st_ml::vad::VadOptions { min_silence_ms: 500.0, ..Default::default() }).unwrap();
    println!("{}", serde_json::to_string(&ch).unwrap());
}
