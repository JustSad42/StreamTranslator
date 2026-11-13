//! For comparing with faster-whisper: writes the mel features of the VAD speech for a 16 kHz f32 file,
//! and transcribes it. Usage: whisper_dump <audio.f32> <mel_out.f32> <hotwords.txt>
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let audio: Vec<f32> = std::fs::read(&a[1]).unwrap().chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    let ch = st_ml::vad::speech_timestamps(&audio, &st_ml::vad::VadOptions { min_silence_ms: 500.0, ..Default::default() }).unwrap();
    let mel = st_ml::mel::Mel::new(&st_ml::vad::collect(&audio, &ch), 128);
    std::fs::write(&a[2], mel.data.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
    st_ml::gpu::setup(false);
    let c = st_core::cancel::Cancel::new();
    let m = st_ml::whisper::WhisperModel::load("large-v3", None, &|_| {}, &|_| {}, &c).unwrap();
    let hot = std::fs::read_to_string(&a[3]).unwrap();
    let segs = m.transcribe(&audio, &st_ml::whisper::Options::new("es", "transcribe", Some(&hot)), &|_| {}, &mut |_| {}, &c).unwrap();
    for s in segs { println!("{}", s.text); }
}
