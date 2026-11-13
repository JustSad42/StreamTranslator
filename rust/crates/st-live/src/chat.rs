//! Live Twitch chat, translated to English on your GPU (chat.py).
//!
//! Reads chat anonymously (Twitch allows read-only "justinfan" logins, no account needed). Messages are
//! translated with NLLB running locally. Emotes are left alone, and messages that are already English
//! are shown as they are. The text filters live in st_core::chat.

use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};
use st_core::{
    cancel::Cancel,
    chat::{parse_line, Message},
};
use st_job::Engines;

use crate::session::Emit;

type Tls = native_tls::TlsStream<TcpStream>;

pub struct ChatSession {
    channel: String,
    size: String,
    stop_event: Cancel,
    queue: Mutex<Vec<Message>>,
    sock: Mutex<Option<TcpStream>>,
    get_lang: Box<dyn Fn() -> Option<String> + Send + Sync>,
    engines: Arc<Engines>,
    emit: Emit,
}

fn msg_json(m: &Message, en: Option<String>) -> Value {
    json!({"id": m.id, "user": m.user, "color": m.color, "text": m.text, "en": en})
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

impl ChatSession {
    pub fn new(channel: &str, size: &str, get_lang: Box<dyn Fn() -> Option<String> + Send + Sync>, engines: Arc<Engines>, emit: Emit) -> Arc<ChatSession> {
        Arc::new(ChatSession {
            channel: channel.to_lowercase().trim_start_matches('#').to_string(),
            size: size.to_string(),
            stop_event: Cancel::new(),
            queue: Mutex::new(vec![]),
            sock: Mutex::new(None),
            get_lang,
            engines,
            emit,
        })
    }

    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        std::thread::spawn(move || me.irc());
        let me = self.clone();
        std::thread::spawn(move || me.worker());
    }

    pub fn stop(&self) {
        self.stop_event.set();
        if let Some(s) = self.sock.lock().unwrap().as_ref() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    fn status(&self, state: &str) {
        (self.emit)("chat.status", json!({"state": state, "channel": self.channel}));
    }

    // ---------------------------------------------------------------- IRC

    fn connect(&self) -> std::io::Result<Tls> {
        let raw = TcpStream::connect_timeout(
            &std::net::ToSocketAddrs::to_socket_addrs(&("irc.chat.twitch.tv", 6697))?.next().ok_or(std::io::ErrorKind::NotFound)?,
            Duration::from_secs(30),
        )?;
        raw.set_read_timeout(Some(Duration::from_secs(300)))?;
        *self.sock.lock().unwrap() = Some(raw.try_clone()?);
        let tls = native_tls::TlsConnector::new().map_err(std::io::Error::other)?;
        tls.connect("irc.chat.twitch.tv", raw).map_err(std::io::Error::other)
    }

    fn irc(&self) {
        let mut backoff = 1u64;
        while !self.stop_event.is_set() {
            if let Ok(mut s) = self.connect() {
                let nick = format!("justinfan{}", 10000 + (now().fract() * 1e6) as u64 % 90000);
                let hello = format!("CAP REQ :twitch.tv/tags\r\nPASS SCHMOOPIIE\r\nNICK {nick}\r\nJOIN #{}\r\n", self.channel);
                if s.write_all(hello.as_bytes()).is_ok() {
                    self.status("connected");
                    backoff = 1;
                    let mut buf: Vec<u8> = vec![];
                    let mut chunk = vec![0u8; 65536];
                    while !self.stop_event.is_set() {
                        let n = match s.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        buf.extend_from_slice(&chunk[..n]);
                        while let Some(i) = buf.windows(2).position(|w| w == b"\r\n") {
                            let line: Vec<u8> = buf.drain(..i + 2).take(i).collect();
                            self.line(&mut s, &String::from_utf8_lossy(&line));
                        }
                    }
                }
            }
            if self.stop_event.is_set() {
                break;
            }
            self.status("reconnecting");
            if self.stop_event.wait(backoff as f64) {
                break;
            }
            backoff = (backoff * 2).min(30);
        }
    }

    fn line(&self, s: &mut Tls, ln: &str) {
        if let Some(rest) = ln.strip_prefix("PING") {
            let _ = s.write_all(format!("PONG{rest}\r\n").as_bytes());
            return;
        }
        let Some(msg) = parse_line(ln, now()) else { return };
        if msg.core.is_none() {
            (self.emit)("chat.message", msg_json(&msg, None));
            return;
        }
        let mut q = self.queue.lock().unwrap();
        if q.len() > 150 {
            // very busy chat: show the oldest untranslated
            let old = q.remove(0);
            (self.emit)("chat.message", msg_json(&old, None));
        }
        q.push(msg);
    }

    // ---------------------------------------------------------------- translation

    fn worker(&self) {
        let emit = self.emit.clone();
        let channel = self.channel.clone();
        let tr = match self.engines.translator(&self.size, &|m| emit("chat.status", json!({"state": "loading", "message": m, "channel": channel})), &|_| {}, &Cancel::new()) {
            Ok(tr) => {
                (self.emit)("chat.status", json!({"state": "ready", "channel": self.channel, "device": tr.device}));
                Some(tr)
            }
            Err(e) => {
                (self.emit)("chat.status", json!({"state": "error", "message": format!("Chat translator failed to load: {e:#}"), "channel": self.channel}));
                None
            }
        };
        while !self.stop_event.wait(0.4) {
            let batch: Vec<Message> = {
                let mut q = self.queue.lock().unwrap();
                let n = q.len().min(24);
                q.drain(..n).collect()
            };
            if batch.is_empty() {
                continue;
            }
            let lang = (self.get_lang)();
            let mut outs: Vec<Option<String>> = vec![None; batch.len()];
            if let (Some(tr), Some(lang)) = (tr.as_ref(), lang.filter(|l| l != "en")) {
                let texts: Vec<String> = batch.iter().map(|m| m.core.clone().unwrap_or_default()).collect();
                if let Ok(r) = tr.translate(&texts, &lang, 4) {
                    outs = r.into_iter().map(Some).collect();
                }
            }
            for (m, en) in batch.iter().zip(outs) {
                let en = en.filter(|e| !e.is_empty() && e.trim().to_lowercase() != m.text.trim().to_lowercase());
                (self.emit)("chat.message", msg_json(m, en));
            }
        }
    }
}
