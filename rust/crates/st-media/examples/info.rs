fn main() {
    let url = std::env::args().nth(1).unwrap();
    let t = std::time::Instant::now();
    let i = st_media::ytdlp::fetch_info(&url).unwrap();
    eprintln!("fetch_info {:.2}s", t.elapsed().as_secs_f64());
    let mut v = serde_json::to_value(&i).unwrap();
    // URLs carry tokens that change per request: compare hosts only
    for (_, s) in v["streams"].as_object_mut().unwrap() { *s = s.as_str().unwrap().split('/').nth(2).unwrap_or("").into(); }
    v["preview_stream"] = "".into(); v["storyboard"] = v["storyboard"]["sheets"].as_array().map(|a| a.len()).into();
    println!("{}", serde_json::to_string(&v).unwrap());
}
