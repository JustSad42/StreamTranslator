//! Port of the app-update and report tests in tests/test_core.py.

use serde_json::json;
use st_appkit::{appupdate, logs};

fn tmpdir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("st-appkit-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn test_version_compare() {
    assert!(appupdate::is_newer("2.2.0", "2.1.0") && appupdate::is_newer("2.10.0", "2.9.9"));
    assert!(!appupdate::is_newer("2.1.0", "2.1.0") && !appupdate::is_newer("2.0.9", "2.1.0"));
}

#[test]
fn test_check_reads_release() {
    let rel = json!({"tag_name": "v9.0.0", "body": "New stuff", "html_url": "https://github.com/x/releases/v9.0.0",
        "assets": [{"name": "notes.txt", "browser_download_url": "u0", "size": 1},
                   {"name": "StreamTranslator-Setup-9.0.0.exe", "browser_download_url": "u1", "size": 123, "digest": "sha256:ABCDEF"}]});
    let info = appupdate::parse(&rel);
    assert!(info["newer"].as_bool().unwrap() && info["latest"] == "9.0.0");
    assert_eq!(info["asset"], json!({"name": "StreamTranslator-Setup-9.0.0.exe", "url": "u1", "size": 123, "sha256": "abcdef"}));
}

#[test]
fn test_download_rejects_bad_checksum() {
    let d = tmpdir("dl");
    let mut asset = json!({"name": "StreamTranslator-Setup-9.0.0.exe", "url": "https://example.invalid/u", "size": 5, "sha256": "0".repeat(64)});
    assert!(appupdate::save_verified(&mut &b"hello"[..], &asset, &d, &|_| {}, None).is_err());
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
    asset["sha256"] = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".into(); // sha256("hello")
    let p = appupdate::save_verified(&mut &b"hello"[..], &asset, &d, &|_| {}, None).unwrap();
    assert_eq!(std::fs::read(p).unwrap(), b"hello");
}

#[test]
fn test_report_link_hides_user_and_fits() {
    let d = tmpdir("log");
    let home = std::env::var("USERPROFILE").unwrap();
    let user = std::path::Path::new(&home).file_name().unwrap().to_string_lossy().to_string();
    let log = d.join("app.log");
    let text: Vec<String> = (0..500).map(|i| format!("line {i} {home}\\Videos\\x.mp4")).collect();
    std::fs::write(&log, text.join("\n")).unwrap();
    let url = logs::report_url_with("It broke", "RTX", &[], &log, None);
    assert!(url.len() < 7500 && !url.contains(&user) && url.contains("It+broke"), "{}", url.len());
}

/// 2.2.0/2.3.0 launched the helper so that PowerShell silently did nothing; updates never installed.
#[test]
fn test_update_helper_really_runs() {
    let d = tmpdir("helper");
    let marker = d.join("ran.txt");
    let mut child = appupdate::run_helper(&format!("Set-Content -Path '{}' -Value ok", marker.display())).unwrap();
    let _ = child.wait();
    for _ in 0..20 {
        if marker.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    assert!(marker.exists());
}

#[test]
fn settings_keep_unknown_keys() {
    let d = tmpdir("settings");
    let p = d.join("settings.json");
    std::fs::write(&p, r#"{"model": "medium", "future_key": 7}"#).unwrap();
    let mut s = st_appkit::settings::Settings::load(&p);
    assert_eq!(s.str("model"), "medium");
    s.save(&json!({"quality": "480p", "not_a_setting": 1})).unwrap();
    let back: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
    assert!(back["future_key"] == 7 && back["quality"] == "480p" && back.get("not_a_setting").is_none());
}
