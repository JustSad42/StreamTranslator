//! StreamTranslator: a WebView2 window (Tauri) showing the interface in streamtranslator/ui, unchanged,
//! plus the backend it talks to (backend.rs).
//!
//! The interface was written for Qt's QWebChannel (`backend.request(json)` + a `backend.event` signal).
//! A small script recreates that object on top of Tauri's invoke/events, so app.js runs as it is.
#![windows_subsystem = "windows"]

mod backend;

use std::sync::Arc;

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WebviewUrl, WebviewWindowBuilder,
};

const SHIM: &str = r#"
(() => {
  const listeners = [];
  window.qt = { webChannelTransport: {} };
  window.QWebChannel = function (_transport, ready) {
    const backend = {
      request: msg => window.__TAURI__.core.invoke('request', { msg }),
      event: { connect: fn => listeners.push(fn) },
    };
    window.__TAURI__.event.listen('backend', e => listeners.forEach(f => f(e.payload))).then(() => ready({ objects: { backend } }));
  };
  // interface size shortcuts (Ctrl+= / Ctrl+- / Ctrl+0), handled by the backend like the Qt app did
  document.addEventListener('keydown', e => {
    if (!e.ctrlKey || e.altKey) return;
    const v = { '=': '+', '+': '+', '-': '-', '0': '0' }[e.key];
    if (!v) return;
    e.preventDefault();
    window.__TAURI__.core.invoke('request', { msg: JSON.stringify({ id: 0, method: 'ui.scale', params: { value: v } }) });
  }, true);
})();
"#;

/// From the floating caption window (live.rs).
#[tauri::command]
fn overlay_cmd(state: tauri::State<'_, Arc<backend::Backend>>, action: String, up: bool, ctrl: bool) {
    state.overlay_cmd(&action, up, ctrl);
}

#[tauri::command]
fn request(state: tauri::State<'_, Arc<backend::Backend>>, msg: String) {
    state.request(msg);
}

fn bring_front(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// `StreamTranslator.exe --selftest <file>`: checks the packaged app's parts and writes PASS/FAIL lines (build-installer.ps1).
fn selftest(out: &str) {
    let mut lines = vec![format!("StreamTranslator {}", st_appkit::VERSION)];
    let mut check = |name: &str, r: anyhow::Result<String>| {
        lines.push(match r {
            Ok(d) => format!("PASS {name}: {d}"),
            Err(e) => format!("FAIL {name}: {e:#}"),
        })
    };
    check("ffmpeg", st_media::tools::ffmpeg_exe().map(|p| p.display().to_string()).map_err(Into::into));
    check("yt-dlp", (|| {
        let exe = st_media::tools::ytdlp_exe()?;
        let out = st_media::tools::command(&exe).arg("--version").output()?;
        anyhow::ensure!(out.status.success(), "yt-dlp --version failed");
        Ok(format!("{} ({})", String::from_utf8_lossy(&out.stdout).trim(), exe.display()))
    })());
    check("ONNX Runtime + voice detection", (|| {
        st_ml::gpu::ort_env()?;
        let p = st_ml::vad::speech_probs(&vec![0f32; 16000])?;
        Ok(format!("{} windows", p.len()))
    })());
    check("WebView2", tauri::webview_version().map_err(|e| anyhow::anyhow!("{e}")));
    st_ml::gpu::setup(false);
    // the GPU libraries are downloaded on first run, so their absence isn't a failure here
    lines.push(format!("INFO CUDA libraries ready: {}", st_ml::gpu::cuda_ready()));
    let _ = std::fs::write(out, lines.join("\r\n") + "\r\n");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 3 && args[1] == "--selftest" {
        selftest(&args[2]);
        return;
    }
    st_appkit::logs::setup();
    unsafe {
        use windows::core::w;
        // lets the installer notice a running copy and ask to close it (same name as the Python app)
        let _ = windows::Win32::System::Threading::CreateMutexW(None, false, w!("StreamTranslatorAppMutex"));
        // same ID as the Start menu shortcut (installer) and notifications (tauri.conf.json "identifier")
        let id: Vec<u16> = "com.justsad42.streamtranslator\0".encode_utf16().collect();
        let _ = windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(windows::core::PCWSTR(id.as_ptr()));
    }
    st_ml::gpu::setup(false);

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![request, overlay_cmd])
        // the floating caption window's page (http://overlay.localhost/)
        .register_uri_scheme_protocol("overlay", |_ctx, _req| {
            tauri::http::Response::builder().header("Content-Type", "text/html; charset=utf-8").body(backend::OVERLAY_PAGE.as_bytes().to_vec()).unwrap()
        })
        .setup(|app| {
            let handle = app.handle().clone();
            let be = Arc::new(backend::Backend::new(handle.clone()));
            app.manage(be.clone());

            let w = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("StreamTranslator")
                .theme(Some(tauri::Theme::Dark))
                .background_color(tauri::webview::Color(9, 9, 13, 255))
                .initialization_script(SHIM)
                .disable_drag_drop_handler() // the page handles dropped links itself
                .additional_browser_args(&backend::browser_args())
                .visible(false)
                .on_navigation(|url| {
                    // normal links open in the real browser instead of inside the app
                    let internal = url.host_str().is_some_and(|h| h.ends_with("localhost")) || url.scheme() == "tauri";
                    if !internal && (url.scheme() == "http" || url.scheme() == "https") {
                        backend::shell_open(url.as_str());
                        return false;
                    }
                    true
                })
                .build()?;
            // open at ~80% of the screen, centred, at the interface size that suits the screen
            if let Ok(Some(m)) = w.current_monitor() {
                let sf = m.scale_factor();
                let area = m.work_area();
                let (aw, ah) = (area.size.width as f64 / sf, area.size.height as f64 / sf);
                let (ww, wh) = (aw * 0.8, ah * 0.85);
                let _ = w.set_size(tauri::LogicalSize::new(ww, wh));
                let _ = w.set_position(tauri::LogicalPosition::new(area.position.x as f64 / sf + (aw - ww) / 2.0, area.position.y as f64 / sf + (ah - wh) / 2.0));
                let z = backend::auto_scale(ah);
                let _ = w.set_min_size(Some(tauri::LogicalSize::new(900.0 * z, 620.0 * z)));
            }
            be.apply_scale();
            w.show()?;

            let open = MenuItem::with_id(app, "open", "Open StreamTranslator", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::with_id("tray")
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("StreamTranslator")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, e| match e.id.as_ref() {
                    "open" => bring_front(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, e| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                        bring_front(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|w, e| {
            if let tauri::WindowEvent::CloseRequested { .. } = e {
                if w.label() != "main" {
                    return;
                }
                w.app_handle().exit(0);
            }
        })
        .build(tauri::generate_context!());
    match app {
        // `run` doesn't return: the exit handler does the tidying up
        Ok(app) => app.run(|h, e| {
            if let tauri::RunEvent::Exit = e {
                if let Some(be) = h.try_state::<Arc<backend::Backend>>() {
                    be.shutdown();
                }
                st_appkit::logs::clean_exit();
            }
        }),
        Err(e) => st_appkit::logs::error(&format!("The window failed to open: {e}")),
    }
}
