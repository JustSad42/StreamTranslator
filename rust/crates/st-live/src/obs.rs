//! OBS browser source (overlay.py ObsServer): a tiny local web server. Add http://127.0.0.1:<port>/ as
//! an OBS Browser Source and captions appear on your own stream. Captions go out as Server-Sent Events.

use std::{
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use serde_json::Value;

/// The page, unchanged from the Python app.
pub const OBS_PAGE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>StreamTranslator captions</title>
<style>
html,body{margin:0;background:transparent;overflow:hidden;font-family:'Segoe UI',system-ui,sans-serif}
#wrap{position:fixed;left:0;right:0;bottom:var(--bottom,6%);display:flex;flex-direction:column;align-items:center;gap:8px;padding:0 4%}
.cap{max-width:100%;text-align:center;color:#fff;font-weight:700;font-size:var(--size,44px);line-height:1.2;
 padding:.25em .6em;border-radius:.35em;background:rgba(8,8,12,var(--bg,.55));
 text-shadow:0 0 4px #000,0 0 2px #000,0 2px 3px #000;animation:in .35s cubic-bezier(.2,.8,.2,1);transition:opacity .6s}
.orig{font-size:calc(var(--size,44px)*.55);font-weight:500;color:#d8d8e4;background:rgba(8,8,12,calc(var(--bg,.55)*.8))}
.partial{opacity:.65}
.gone{opacity:0}
@keyframes in{from{opacity:0;transform:translateY(12px) scale(.98)}to{opacity:1;transform:none}}
</style></head><body><div id="wrap"></div>
<script>
const q=new URLSearchParams(location.search), root=document.documentElement;
if(q.get('size'))root.style.setProperty('--size',q.get('size')+'px');
if(q.get('bg'))root.style.setProperty('--bg',q.get('bg'));
if(q.get('bottom'))root.style.setProperty('--bottom',q.get('bottom')+'%');
const showOrig=q.get('orig')!=='0', wrap=document.getElementById('wrap'); let timer;
function render(m){
  wrap.innerHTML='';
  if(m.text){const d=document.createElement('div');d.className='cap'+(m.partial?' partial':'');d.textContent=m.text;wrap.appendChild(d)}
  if(showOrig&&m.orig){const o=document.createElement('div');o.className='cap orig';o.textContent=m.orig;wrap.appendChild(o)}
  clearTimeout(timer);timer=setTimeout(()=>wrap.querySelectorAll('.cap').forEach(c=>c.classList.add('gone')),10000);
}
function connect(){const es=new EventSource('/events');es.onmessage=e=>render(JSON.parse(e.data));es.onerror=()=>{es.close();setTimeout(connect,2000)}}
connect();
</script></body></html>"#;

#[derive(Default)]
struct Shared {
    clients: Vec<mpsc::Sender<String>>,
    last: Option<String>,
}

pub struct ObsServer {
    pub port: u16,
    shared: Arc<Mutex<Shared>>,
}

impl ObsServer {
    /// Listen on 127.0.0.1, trying `port` and the next 9 if it's taken.
    pub fn start(port: u16) -> Result<ObsServer> {
        let mut last_err = None;
        for p in port..port + 10 {
            match TcpListener::bind(("127.0.0.1", p)) {
                Ok(l) => {
                    let shared = Arc::new(Mutex::new(Shared::default()));
                    let sh = shared.clone();
                    std::thread::spawn(move || {
                        for conn in l.incoming().flatten() {
                            let sh = sh.clone();
                            std::thread::spawn(move || {
                                let _ = handle(conn, &sh);
                            });
                        }
                    });
                    return Ok(ObsServer { port: p, shared });
                }
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap().into())
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    /// {text, orig, partial} to every open browser source.
    pub fn send(&self, msg: &Value) {
        let data = msg.to_string();
        let mut sh = self.shared.lock().unwrap();
        sh.last = Some(data.clone());
        sh.clients.retain(|c| c.send(data.clone()).is_ok());
    }
}

fn handle(mut conn: TcpStream, sh: &Mutex<Shared>) -> std::io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(conn.try_clone()?);
    let mut first = String::new();
    reader.read_line(&mut first)?;
    // skip the headers
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h == "\r\n" || h == "\n" {
            break;
        }
    }
    let path = first.split_whitespace().nth(1).unwrap_or("/");
    if path.starts_with("/events") {
        conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n")?;
        let (tx, rx) = mpsc::channel::<String>();
        let last = {
            let mut s = sh.lock().unwrap();
            s.clients.push(tx);
            s.last.clone()
        };
        if let Some(l) = last {
            conn.write_all(format!("data: {l}\n\n").as_bytes())?;
        }
        loop {
            match rx.recv_timeout(Duration::from_secs(15)) {
                Ok(m) => conn.write_all(format!("data: {m}\n\n").as_bytes())?,
                Err(mpsc::RecvTimeoutError::Timeout) => conn.write_all(b": ping\n\n")?,
                Err(_) => return Ok(()),
            }
            conn.flush()?;
        }
    }
    let body = OBS_PAGE.as_bytes();
    conn.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes())?;
    conn.write_all(body)
}
