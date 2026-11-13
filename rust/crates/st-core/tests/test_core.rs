//! Port of tests/test_core.py (the parts that live in st-core). Same cases, same expectations.

use std::collections::HashSet;

use st_core::{
    cluster::cluster,
    dsp::audio_offset,
    glossary::Glossary,
    ocrtext::{clock_values, minute_variants, players_in},
    subs::{balanced_wrap, read_srt, split_sentences, to_cues, write_srt, write_srt_cues, Cue, Segment},
    sync::{classify, map_times, vod_covering, Kind, VodEntry},
    timefmt::{format_hms_tag, format_ts, parse_ts, srt_ts},
    vod::{vod_link, VodInfo, KICK_VOD},
};

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("st-core-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

// ---------------------------------------------------------------- time
#[test]
fn test_parse_ts() {
    for (text, secs) in [("1:02:30", 3750.0), ("62:30", 3750.0), ("3750", 3750.0), ("1h2m30s", 3750.0), ("45m", 2700.0), ("0:05", 5.0)] {
        assert_eq!(parse_ts(text), Ok(secs), "{text}");
    }
}

#[test]
fn test_parse_ts_rejects() {
    for bad in ["", "1::2", "abc", "1:2:3:4"] {
        assert!(parse_ts(bad).is_err(), "{bad}");
    }
}

#[test]
fn test_format_and_tags() {
    assert_eq!(format_ts(3750.0), "1:02:30");
    assert_eq!(format_ts(95.0), "1:35");
    assert_eq!(format_hms_tag(3750.0), "1h02m30s");
    assert_eq!(srt_ts(3750.5), "01:02:30,500");
}

// ---------------------------------------------------------------- glossary
fn gl() -> Glossary {
    Glossary::defaults()
}

#[test]
fn test_glossary_fixes() {
    let g = gl();
    for (src, want) in [
        ("Nafiri flashes in", "Naafiri flashes in"),
        ("Casante dies", "K'Sante dies"),
        ("they take the Heraldo", "they take the Herald"),
        ("Kaisa is fed", "Kai'Sa is fed"),
        ("Lee Shin goes in", "Lee Sin goes in"),
        ("the Void Larvae", "the Void Grubs"),
        ("He used the ulti", "He used the ult"),
    ] {
        assert_eq!(g.fix(src, true, &[]).0, want);
    }
}

#[test]
fn test_glossary_leaves_english_alone() {
    let g = gl();
    for text in ["There is a brand new strategy", "Thank you Victor", "Graves and Viktor are strong"] {
        assert_eq!(g.fix(text, true, &[]).0, text);
    }
}

#[test]
fn test_glossary_extra_names() {
    assert_eq!(gl().fix("Fakker played well", true, &s(&["Faker"])).0, "Faker played well");
}

#[test]
fn test_hotwords_without_core_terms() {
    let hw = gl().hotwords(&s(&["Orianna"]), false, &s(&["Faker"])).unwrap();
    assert!(hw.starts_with("Orianna, Faker") && !hw.contains("Rift Herald"));
}

#[test]
fn test_prompt_echo() {
    let g = gl();
    let hw = g.hotwords(&s(&["Orianna", "Nocturne"]), true, &[]);
    assert!(g.is_prompt_echo("Orianna, Nocturne, Baron, Dragon, Elder Dragon", hw.as_deref()));
    assert!(!g.is_prompt_echo("Orianna goes for the Baron, nice", hw.as_deref()));
}

// ---------------------------------------------------------------- sync
#[test]
fn test_classify() {
    for (text, kind) in [
        ("ibai", Kind::Channel),
        ("twitch.tv/caedrel", Kind::Channel),
        ("2923514362", Kind::Vod),
        ("https://www.twitch.tv/videos/2923514362?t=1h", Kind::Vod),
        ("https://youtube.com/watch?v=x", Kind::Url),
    ] {
        assert_eq!(classify(text).unwrap().0, kind, "{text}");
    }
}

fn vod(id: &str, start_ts: f64, duration: f64) -> VodInfo {
    VodInfo {
        url: format!("https://www.twitch.tv/videos/{id}"),
        id: id.into(),
        title: "t".into(),
        channel: format!("ch{id}"),
        duration,
        streams: [("Source".to_string(), "x".to_string())].into(),
        preview_stream: "x".into(),
        start_ts: Some(start_ts),
        site: "twitch".into(),
        ..Default::default()
    }
}

#[test]
fn test_sync_maps_times() {
    let mine = vod("1", 1_000_000.0, 4.0 * 3600.0);
    let theirs = vod("2", 1_000_000.0 - 600.0, 5.0 * 3600.0); // they went live 10 min earlier
    let r = map_times(&mine, 3600.0, 3700.0, &theirs).unwrap();
    assert!(r.offset == 600.0 && r.start == 4200.0 && r.end == 4300.0 && !r.clipped);
}

#[test]
fn test_sync_rejects_uncovered_moment() {
    let mine = vod("1", 1_000_000.0, 4.0 * 3600.0);
    let theirs = vod("2", 1_000_000.0 + 7200.0, 600.0); // live later, briefly
    assert!(map_times(&mine, 60.0, 120.0, &theirs).is_err());
}

#[test]
fn test_vod_covering() {
    let v = |st: f64| VodEntry { start_ts: Some(st), duration: 500.0, ..Default::default() };
    let vods = vec![v(1000.0), v(5000.0)];
    assert!(std::ptr::eq(vod_covering(&vods, 5200.0).unwrap(), &vods[1]));
    assert!(vod_covering(&vods, 3000.0).is_none());
}

// ---------------------------------------------------------------- snap (game clock)
#[test]
fn test_clock_values_fix_ocr_letters() {
    assert_eq!(clock_values(&s(&["• 15:12", "1:37", "138.339"])), vec![912, 97]);
    assert_eq!(clock_values(&s(&["2O:O5"])), vec![1205]);
    assert_eq!(clock_values(&s(&["1:02:30"])), Vec::<u32>::new()); // h:mm:ss is stream uptime
}

#[test]
fn test_minute_variants_cover_misreads() {
    assert!(minute_variants(21 * 60 + 22).contains(&(27 * 60 + 22)));
    assert!(minute_variants(28 * 60 + 22).contains(&(26 * 60 + 22)));
}

// ---------------------------------------------------------------- translate / subtitles
#[test]
fn test_split_sentences() {
    assert_eq!(split_sentences("Ya, ya. No, literal."), s(&["Ya, ya.", "No, literal."]));
    assert_eq!(split_sentences("one line"), s(&["one line"]));
}

#[test]
fn test_balanced_wrap() {
    let out = balanced_wrap("They're fighting 5v4. Heraldo is at 6,000 HP.", 42);
    let (a, b) = out.split_once('\n').unwrap();
    assert!((a.len() as i64 - b.len() as i64).abs() < 12);
}

#[test]
fn test_cues_split_long_segments() {
    let cues = to_cues(&[Segment::new(0.0, 10.0, "word ".repeat(60))]);
    assert!(cues.len() > 1 && cues.iter().all(|c| c.text.matches('\n').count() <= 1));
    assert!(cues[0].start == 0.0 && (cues.last().unwrap().end - 10.0).abs() < 0.01);
}

#[test]
fn test_srt_roundtrip() {
    let cue = |a: f64, b: f64, t: &str| Cue { start: a, end: b, text: t.into(), color: None };
    let p = tmp("x.srt");
    write_srt_cues(&[cue(1.0, 2.5, "Hello"), cue(0.2, 0.9, "First"), cue(3.0, 4.0, "   ")], &p).unwrap();
    let back = read_srt(&p).unwrap();
    assert_eq!(back.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), vec!["First", "Hello"]);
    assert!(back[1].start == 1.0 && back[1].end == 2.5);
}

// ---------------------------------------------------------------- detection
#[test]
fn test_players_from_ocr() {
    let frames = vec![
        s(&["Tl Faker", "BLG Knight", "Xun", "verizon", "NEARSIGHTED", "LCK#3"]),
        s(&["T1 Faker", "Xun", "BLG_Knight", "PATCH 26.20"]),
        s(&["Tl_oner", "Xun", "Faker"]),
    ];
    let names = players_in(&frames, &gl().champion_keys());
    for n in ["Faker", "Knight", "Xun"] {
        assert!(names.contains(&n.to_string()), "{names:?}");
    }
    for n in ["Verizon", "Nearsighted", "Patch", "Lck"] {
        assert!(!names.contains(&n.to_string()), "{names:?}");
    }
}

// ---------------------------------------------------------------- 2.3: slang, speakers, timing, links
#[test]
fn test_slang_prepare() {
    let g = gl();
    for (src, lang, want) in [
        ("Qué jugadón de Faker", "es", "Qué great play de Faker"),
        ("El equipo fue de menos a más.", "es", "El equipo fue getting better."),
        ("Es un manco", "es", "Es a bad player"),
        ("Rushea el dragón", "es", "Rush el dragón"),
        ("O Faker amassou", "pt", "O Faker stomped"),
        ("mancomunidad", "es", "mancomunidad"), // whole words only
        ("Qué jugadón", "en", "Qué jugadón"),   // only that language's list
    ] {
        assert_eq!(g.prepare(src, Some(lang)).0, want);
    }
}

/// Deterministic normal-ish noise (the Python test uses numpy's generator; the property is what matters).
struct Rng(u64);
impl Rng {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn normal(&mut self) -> f64 {
        let (u, v) = (self.uniform().max(1e-12), self.uniform());
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()
    }
}

#[test]
fn test_speaker_groups() {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let a: Vec<f64> = (0..256).map(|_| rng.normal()).collect();
    let b: Vec<f64> = (0..256).map(|_| rng.normal()).collect();
    let mut rows: Vec<Vec<f32>> = vec![];
    for (base, count) in [(&a, 12), (&b, 6)] {
        for _ in 0..count {
            let r: Vec<f64> = base.iter().map(|x| x + rng.normal() * 1.2).collect();
            let n = r.iter().map(|v| v * v).sum::<f64>().sqrt();
            rows.push(r.iter().map(|v| (v / n) as f32).collect());
        }
    }
    let labels = cluster(&rows, &[1.0; 18], 0.3);
    let first: HashSet<_> = labels[..12].iter().collect();
    let second: HashSet<_> = labels[12..].iter().collect();
    assert!(first.len() == 1 && second.len() == 1 && labels[0] != labels[12], "{labels:?}");
}

#[test]
fn test_audio_offset_finds_shift() {
    let mut rng = Rng(12345);
    let r: Vec<f32> = (0..16000 * 20).map(|_| rng.normal() as f32).collect();
    let mut later: Vec<f32> = (0..24000).map(|_| rng.normal() as f32).collect(); // video starts 1.5 s earlier
    later.extend(&r);
    assert!((audio_offset(&r, &later, 15.0) - 1.5).abs() < 0.01);
    assert!((audio_offset(&later[24000 + 8000..], &later[24000..], 15.0) - 0.5).abs() < 0.01);
}

#[test]
fn test_speaker_colours_survive_editing() {
    let p = tmp("s.srt");
    let mut b = Segment::new(2.0, 4.0, "Qué tal");
    b.speaker = 1;
    write_srt(&[Segment::new(0.0, 2.0, "Hola"), b], &p, 0.5).unwrap();
    let cues = read_srt(&p).unwrap();
    assert!(cues[0].text == "Hola" && cues[0].color.is_none());
    assert!(cues[1].text == "Qué tal" && cues[1].color.as_deref().unwrap().starts_with('#'));
    assert_eq!(cues[0].start, 0.5); // shifted to match the video
    write_srt_cues(&cues, &p).unwrap();
    assert_eq!(read_srt(&p).unwrap()[1].color, cues[1].color);
}

#[test]
fn test_vod_links() {
    let t = vod("123", 0.0, 100.0);
    assert_eq!(vod_link(&t, 3750.0), "https://www.twitch.tv/videos/123?t=1h02m30s");
    let y = VodInfo { url: "https://www.youtube.com/watch?v=abcdefghijk".into(), id: "abcdefghijk".into(), site: "youtube".into(), ..Default::default() };
    assert_eq!(vod_link(&y, 75.9), "https://www.youtube.com/watch?v=abcdefghijk&t=75s");
}

#[test]
fn test_kick_links_recognised() {
    assert!(KICK_VOD.is_match("https://kick.com/xqc/videos/740b79a4-780d-422a-8b97-6a260148e8ba"));
    assert!(!KICK_VOD.is_match("https://kick.com/xqc"));
}

// ---------------------------------------------------------------- chat filters

#[test]
fn test_strip_laughs() {
    use st_core::chat::strip_laughs;
    for (src, want) in [
        ("JAJAJAJAJA", ""), ("JSJWKSKKAKAKA", ""), ("jajaja que malo es", "que malo es"), ("kkkkkk mano", "mano"),
        ("rsrsrs que isso", "que isso"), ("hijo de su madre", "hijo de su madre"), ("esas hojas", "esas hojas"),
        ("ahi esta", "ahi esta"), ("Jaime ganó", "Jaime ganó"),
    ] {
        assert_eq!(strip_laughs(src), want, "{src}");
    }
}

#[test]
fn test_is_english() {
    assert!(st_core::chat::is_english("lol that was so good bro"));
    assert!(!st_core::chat::is_english("que jugada mas buena hermano"));
}

#[test]
fn test_chat_line_parsing() {
    use st_core::chat::parse_line;
    let m = parse_line("@color=#FF0000;display-name=Pepe;emotes=25:0-4;id=abc :pepe!pepe@pepe.tmi.twitch.tv PRIVMSG #ibai :Kappa jajaja que jugada mas buena KEKW", 1.0).unwrap();
    assert_eq!((m.id.as_str(), m.user.as_str(), m.color.as_str()), ("abc", "Pepe", "#FF0000"));
    assert_eq!(m.core.as_deref(), Some("que jugada mas buena"));
    let en = parse_line(":bob!bob@bob.tmi.twitch.tv PRIVMSG #ibai :\x01ACTION lol that was so good bro\x01", 2.5).unwrap();
    assert_eq!((en.user.as_str(), en.text.as_str(), en.core, en.id.as_str()), ("bob", "lol that was so good bro", None, "2.5"));
    assert!(parse_line("PING :tmi.twitch.tv", 0.0).is_none());
}
