//! ffmpeg: section downloads, audio decoding, frame grabs and subtitle burn-in (pipeline.py, isolate.py).

use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::Stdio,
};

use anyhow::Result;
use st_core::{cancel::Cancel, job_err, vod::VodInfo, Cancelled, JobError};

use crate::{
    tools::{command, ffmpeg_exe},
    Progress,
};

fn ua_args(ua: &str) -> Vec<String> {
    if ua.is_empty() {
        vec![]
    } else {
        vec!["-user_agent".into(), ua.into()]
    }
}

/// Run ffmpeg with `-progress`, reporting 0..1 against `duration` seconds. Cancelling kills it.
pub fn run_ffmpeg(args: &[String], duration: f64, progress: Progress, cancel: &Cancel, cwd: Option<&Path>) -> Result<()> {
    let exe = ffmpeg_exe()?;
    let mut cmd = command(&exe);
    cmd.args(["-hide_banner", "-nostdin", "-y", "-loglevel", "error", "-progress", "pipe:1", "-nostats"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let mut child = cmd.spawn()?;
    let mut stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let stdout = BufReader::new(child.stdout.take().unwrap());
    for line in stdout.lines() {
        let Ok(line) = line else { break };
        if cancel.is_set() {
            let _ = child.kill();
            break;
        }
        if let Some(v) = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms=")) {
            if let Ok(us) = v.trim().parse::<i64>() {
                if duration > 0.0 {
                    progress((us as f64 / 1e6 / duration).min(1.0));
                }
            }
        }
    }
    let status = child.wait()?;
    let err = err_thread.join().unwrap_or_default();
    if cancel.is_set() {
        return Err(Cancelled.into());
    }
    if !status.success() {
        let tail: Vec<&str> = err.lines().collect();
        let tail = tail[tail.len().saturating_sub(15)..].join("\n");
        return job_err(format!("ffmpeg failed:\n{}", tail.trim()));
    }
    Ok(())
}

/// Download `start..end` of the VOD with stream copy. `audio_only` takes the sound-only rendition
/// (accurate start); video snaps to a keyframe, which the job measures and corrects later.
pub fn download_section(info: &VodInfo, quality: &str, start: f64, end: f64, out: &Path, progress: Progress, cancel: &Cancel, audio_only: bool) -> Result<()> {
    let dur = end - start;
    let ss = vec!["-ss".to_string(), format!("{start:.3}")];
    let (inputs, maps): (Vec<String>, Vec<&str>) = if audio_only {
        let url = info.streams.get("Audio").or_else(|| info.streams.get("480p")).or_else(|| info.streams.get("Source")).cloned().unwrap_or_default();
        ([ss.clone(), vec!["-i".into(), url]].concat(), vec!["-map", "0:a:0", "-vn"])
    } else {
        let q = if info.streams.contains_key(quality) { quality } else { "Source" };
        let url = info.streams.get(q).cloned().unwrap_or_default();
        if info.video_only.contains(q) {
            // YouTube: picture and sound are separate streams
            let audio = info.streams.get("Audio").cloned().unwrap_or_default();
            ([ss.clone(), vec!["-i".into(), url], ua_args(&info.user_agent), ss.clone(), vec!["-i".into(), audio]].concat(), vec!["-map", "0:v:0", "-map", "1:a:0"])
        } else {
            ([ss.clone(), vec!["-i".into(), url]].concat(), vec!["-map", "0:v:0?", "-map", "0:a:0?"])
        }
    };
    // the .mp4/.m4a writer adds the ADTS->ASC fix for AAC by itself (and skips it for Opus)
    let mut args = ua_args(&info.user_agent);
    args.extend(inputs);
    args.extend(["-t".into(), format!("{dur:.3}")]);
    args.extend(maps.iter().map(|s| s.to_string()));
    let local = crate::local::is_local(info);
    // a local file's sound can be anything (PCM, Vorbis, AC-3...), so it's converted to AAC for the .mp4/.m4a
    let codecs: &[&str] = if local { &["-c:v", "copy", "-c:a", "aac", "-b:a", "192k"] } else { &["-c", "copy"] };
    let tail = |codecs: &[&str]| -> Vec<String> {
        let mut a: Vec<String> = codecs.iter().map(|s| s.to_string()).collect();
        a.extend(["-avoid_negative_ts", "make_zero", "-movflags", "+faststart"].iter().map(|s| s.to_string()));
        a.push(out.to_string_lossy().into_owned());
        a
    };
    let copy = [args.clone(), tail(codecs)].concat();
    match run_ffmpeg(&copy, dur, progress, cancel, None) {
        // a picture format .mp4 can't hold (ProRes, some AVI codecs...): re-encode it instead
        Err(e) if local && !audio_only && e.downcast_ref::<Cancelled>().is_none() => {
            let _ = std::fs::remove_file(out);
            let enc = ["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "192k"];
            run_ffmpeg(&[args, tail(&enc)].concat(), dur, progress, cancel, None).map_err(|e2| anyhow::anyhow!("{e2}
(copying first: {e})"))?;
        }
        r => r?,
    }
    if std::fs::metadata(out).map(|m| m.len()).unwrap_or(0) < 10_000 {
        return job_err(if crate::local::is_local(info) {
            "Cutting the section out of the video file came back empty. Is that part of the file readable?"
        } else {
            "The download came back empty. The VOD may be muted, deleted, or sub-only."
        });
    }
    Ok(())
}

/// Raw float32 PCM from ffmpeg. `input` args go before the output options; `stdin` is fed to ffmpeg.
pub fn pcm(input: &[String], stdin: Option<&[u8]>, rate: u32, channels: u32) -> Result<Vec<f32>> {
    let exe = ffmpeg_exe()?;
    let mut cmd = command(&exe);
    cmd.args(["-hide_banner", "-loglevel", "error"]);
    if stdin.is_none() {
        cmd.arg("-nostdin");
    }
    cmd.args(input)
        .args(["-vn", "-ac", &channels.to_string(), "-ar", &rate.to_string(), "-f", "f32le", "-"])
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let feeder = stdin.map(|data| {
        let mut w = child.stdin.take().unwrap();
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = w.write_all(&data);
        })
    });
    let out = child.wait_with_output()?;
    if let Some(f) = feeder {
        let _ = f.join();
    }
    if !out.status.success() {
        let e = String::from_utf8_lossy(&out.stderr);
        let tail: String = e.chars().rev().take(800).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(JobError(format!("Audio conversion failed:\n{tail}")).into());
    }
    Ok(out.stdout.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

/// 16 kHz mono float32 samples (what Whisper wants).
pub fn decode_audio(path: &Path) -> Result<Vec<f32>> {
    let audio = pcm(&["-i".into(), path.to_string_lossy().into_owned()], None, 16000, 1)
        .map_err(|e| JobError(format!("Couldn't read the clip's audio:\n{e}")))?;
    if audio.len() < 16000 {
        return job_err("The clip has no audio track.");
    }
    Ok(audio)
}

pub(crate) fn grab(info: &VodInfo, stream: &str, t: f64, vf: &str, codec: &str, timeout_s: u64) -> Result<Vec<u8>> {
    let exe = ffmpeg_exe()?;
    let mut cmd = command(&exe);
    cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error"])
        .args(ua_args(&info.user_agent))
        .args(["-ss", &format!("{:.2}", t.max(0.0)), "-i", stream, "-frames:v", "1", "-vf", vf, "-f", "image2pipe", "-vcodec", codec, "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut out = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut b = vec![];
        let _ = out.read_to_end(&mut b);
        b
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_s);
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let bytes = reader.join().unwrap_or_default();
    match status {
        Some(s) if s.success() && !bytes.is_empty() => Ok(bytes),
        _ => {
            let mut e = String::new();
            if let Some(mut se) = child.stderr.take() {
                let _ = se.read_to_string(&mut e);
            }
            job_err(if e.trim().is_empty() { "frame grab failed".to_string() } else { e.trim().to_string() })
        }
    }
}

/// One JPEG frame from the VOD (preview thumbnails, thumb.jpg).
pub fn grab_frame(info: &VodInfo, seconds: f64, width: u32) -> Result<Vec<u8>> {
    // the last frame starts a little before the end: asking for the very end returns nothing
    let seconds = if info.duration > 2.0 { seconds.min(info.duration - 1.0) } else { seconds };
    grab(info, &info.preview_stream, seconds, &format!("scale={width}:-2"), "mjpeg", 30)
}

/// One PNG frame from the best stream (champion/player detection).
pub fn frame_png(info: &VodInfo, t: f64, width: u32) -> Result<Vec<u8>> {
    grab(info, info.source_stream(), t, &format!("scale={width}:-2"), "png", 40)
}

/// Top 22% of the frame (where League shows the game clock), high contrast and enlarged for OCR.
pub fn top_strip(info: &VodInfo, t: f64) -> Result<Vec<u8>> {
    grab(info, info.source_stream(), t, "scale=1920:-2,crop=iw:ih*0.22:0:0,format=gray,negate,eq=contrast=1.5,scale=iw*2:-2:flags=lanczos", "png", 40)
}

/// Subtitle placement: label -> (ASS alignment, vertical margin on a 288-line canvas).
pub const POSITIONS: &[(&str, i32, i32)] = &[("Above game HUD", 2, 46), ("Bottom", 2, 12), ("Top", 8, 14)];
pub const FONT_SIZES: &[(&str, i32)] = &[("Small", 12), ("Medium", 14), ("Large", 17)];

/// Burn `srt` into `clip`, writing `out` (same folder). NVENC first, libx264 if the GPU encoder fails.
pub fn burn_subtitles(clip: &Path, srt: &Path, out: &Path, position: &str, font_size: &str, duration: f64, progress: Progress, log: &(dyn Fn(&str) + Sync), cancel: &Cancel) -> Result<()> {
    let (_, align, margin) = POSITIONS.iter().find(|p| p.0 == position).copied().unwrap_or(POSITIONS[0]);
    let size = FONT_SIZES.iter().find(|f| f.0 == font_size).map(|f| f.1).unwrap_or(14);
    let style = format!(
        "FontName=Arial,FontSize={size},Bold=1,PrimaryColour=&H00FFFFFF,OutlineColour=&H00000000,BackColour=&H80000000,BorderStyle=1,Outline=1.6,Shadow=0.6,Alignment={align},MarginV={margin}"
    );
    // Run inside the output folder with a plain file name: avoids ffmpeg's filter-path escaping
    // problems with drive letters and odd characters.
    let dir = clip.parent().unwrap();
    let tmp = dir.join("_burn.srt");
    std::fs::copy(srt, &tmp)?;
    let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
    let common: Vec<String> = vec!["-i".into(), name(clip), "-vf".into(), format!("subtitles=_burn.srt:force_style='{style}'")];
    let tail: Vec<String> = ["-pix_fmt", "yuv420p", "-c:a", "copy", "-movflags", "+faststart"].iter().map(|s| s.to_string()).chain([name(out)]).collect();
    let gpu: Vec<String> = ["-c:v", "h264_nvenc", "-preset", "p5", "-tune", "hq", "-rc", "vbr", "-cq", "21", "-b:v", "0"].iter().map(|s| s.to_string()).collect();
    let cpu: Vec<String> = ["-c:v", "libx264", "-preset", "veryfast", "-crf", "20"].iter().map(|s| s.to_string()).collect();
    let r = (|| -> Result<()> {
        match run_ffmpeg(&[common.clone(), gpu, tail.clone()].concat(), duration, progress, cancel, Some(dir)) {
            Err(e) if e.downcast_ref::<JobError>().is_some() => {
                let last = e.to_string().lines().last().unwrap_or("").chars().take(120).collect::<String>();
                log(&format!("GPU encoder unavailable, using CPU encoder. ({last})"));
                run_ffmpeg(&[common.clone(), cpu, tail.clone()].concat(), duration, progress, cancel, Some(dir))
            }
            r => r,
        }
    })();
    let _ = std::fs::remove_file(&tmp);
    r
}
