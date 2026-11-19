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

/// The page. Its look comes from the app (a {"style": ...} event), so changes show at once in OBS;
/// ?demo shows sample lines (the preview in the app).
pub const OBS_PAGE: &str = r##"<!doctype html><html><head><meta charset="utf-8"><title>StreamTranslator captions</title>
<style>
:root{--size:42px;--bg:.6;--pos:7%;--width:80%;--font:'Segoe UI',system-ui,sans-serif;--stroke:.09em;--lh:1.25}
html,body{margin:0;background:transparent;overflow:hidden}
#wrap{position:fixed;left:0;right:0;bottom:var(--pos);display:flex;flex-direction:column;align-items:center;gap:calc(var(--size)*.22);padding:0 3%;font-family:var(--font)}
body.top #wrap{bottom:auto;top:var(--pos);flex-direction:column}
body.left #wrap{align-items:flex-start}
.cap{max-width:var(--width);box-sizing:border-box;text-align:center;color:#fff;font-weight:700;font-size:var(--size);line-height:var(--lh);
 padding:.22em .62em .26em;border-radius:.38em;background:rgba(10,10,16,var(--bg));
 -webkit-text-stroke:var(--stroke) rgba(0,0,0,.85);paint-order:stroke fill;
 text-shadow:0 .04em .12em rgba(0,0,0,.9),0 0 .3em rgba(0,0,0,.45);
 animation:in .38s cubic-bezier(.2,.8,.2,1) backwards;transition:opacity .7s ease,transform .7s ease;overflow-wrap:anywhere}
body.left .cap{text-align:left}
body.nostroke .cap{-webkit-text-stroke:0}
body.box .cap{backdrop-filter:blur(6px)}
.orig{font-size:calc(var(--size)*.56);font-weight:500;color:#dcdce8;background:rgba(10,10,16,calc(var(--bg)*.8))}
.partial{opacity:.65}
.team{font-size:var(--size)}
.who{font-weight:800;margin-right:.18em}
.av{width:1.1em;height:1.1em;border-radius:50%;object-fit:cover;vertical-align:-.18em;margin-right:.28em;box-shadow:0 0 0 .05em rgba(0,0,0,.7),0 .04em .14em rgba(0,0,0,.6)}
body.noav .av{display:none}
.cont{padding-top:.12em}
body.names-white .who{color:#fff!important}
body.names-off .who{display:none}
body.names-above .cap.team{display:flex;flex-direction:column;align-items:center;gap:.06em}
body.names-above.left .cap.team{align-items:flex-start}
body.names-above .who{font-size:.68em;letter-spacing:.04em;text-transform:uppercase;margin:0;-webkit-text-stroke:calc(var(--stroke)*.8) rgba(0,0,0,.85);display:inline-flex;align-items:center}
body.names-above .av{width:1.5em;height:1.5em;vertical-align:0}
.old{opacity:.72}
.gone{opacity:0;transform:translateY(6px)}
@keyframes in{from{opacity:0;transform:translateY(14px) scale(.97)}to{opacity:1;transform:none}}
</style></head><body><div id="wrap"></div>
<script>
const q=new URLSearchParams(location.search), root=document.documentElement, wrap=document.getElementById('wrap');
const S={size:42,bg:.6,pos:7,place:'bottom',align:'center',lines:3,hide:12,names:'color',stroke:true,width:80,font:'Segoe UI',orig:true,fade:true,avatars:true,split:true};
// old links: ?size=56&bg=0.4&bottom=10&orig=0
if(q.get('size'))S.size=+q.get('size'); if(q.get('bg'))S.bg=+q.get('bg'); if(q.get('bottom'))S.pos=+q.get('bottom'); if(q.get('orig')==='0')S.orig=false;
let last=null, timer;
const face=(c,l)=>'data:image/svg+xml,'+encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><circle cx="32" cy="32" r="32" fill="${c}"/><text x="32" y="43" font-size="30" text-anchor="middle" font-family="Segoe UI,sans-serif" font-weight="800" fill="#16161e">${l}</text></svg>`);
const DEMO={lines:[{name:'Ahmed',color:'#ffd166',avatar:face('#ffd166','A'),text:'Flash is down on their ADC, go now!'},{name:'Omar',color:'#ff7aa8',avatar:face('#ff7aa8','O'),text:'I have ultimate, I can engage on the jungler, so wait for my call and then everyone follow up behind me.'},{name:'Revenger',color:'#7cc4ff',avatar:face('#7cc4ff','R'),text:"Wait for me, I'm coming from top side."}]};
// a long line reads badly at stream size: cut it at a sentence end, a comma, or a space near the middle
const MAXC=68;
function chunks(t){
  if(!S.split||t.length<=MAXC)return [t];
  const mid=t.length/2;let best=-1,score=1e9;
  for(let i=Math.floor(t.length*.3);i<t.length*.72;i++){const c=t[i];if(c!==' ')continue;const p=t[i-1];
    const w=/[.!?]/.test(p)?0:/[,;:]/.test(p)?.18:.45;const sc=Math.abs(i-mid)/t.length+w;if(sc<score){score=sc;best=i}}
  if(best<0)return [t];
  return [...chunks(t.slice(0,best).trim()),...chunks(t.slice(best+1).trim())];
}
function applyStyle(){
  root.style.setProperty('--size',S.size+'px'); root.style.setProperty('--bg',S.bg); root.style.setProperty('--pos',S.pos+'%');
  root.style.setProperty('--width',S.width+'%'); root.style.setProperty('--font',`'${S.font}','Segoe UI',system-ui,sans-serif`);
  document.body.className=[S.place==='top'?'top':'',S.align==='left'?'left':'',S.stroke?'':'nostroke','names-'+S.names,S.avatars===false?'noav':''].join(' ');
}
function render(m){
  wrap.innerHTML=''; clearTimeout(timer);
  if(!m)return;
  // captions sent earlier (a style change, OBS reconnecting) keep their original hide time
  const demo=q.has('demo'), left=S.hide>0?S.hide*1000-(m.at?Date.now()-m.at:0):1;
  if(!demo&&left<=0)return;
  if(m.lines){
    const lines=m.lines.slice(-Math.max(1,S.lines));
    const list=S.place==='top'?lines.slice().reverse():lines;
    list.forEach((l,i)=>{
      const newest=S.place==='top'?i===0:i===list.length-1;
      chunks(l.text||'').forEach((part,k)=>{
        const d=document.createElement('div');d.className='cap team'+(k?' cont':'');
        if(S.fade&&!newest&&list.length>1)d.classList.add('old');
        if(!k){const n=document.createElement('span');n.className='who';n.style.color=l.color;
          if(l.avatar){const a=document.createElement('img');a.className='av';a.src=l.avatar;a.alt='';a.onerror=()=>a.remove();n.appendChild(a)}
          n.appendChild(document.createTextNode(S.names==='above'?l.name:l.name+':'));d.appendChild(n)}
        const t=document.createElement('span');t.textContent=part;d.appendChild(t);wrap.appendChild(d)});
    });
  }else{
    if(m.text){const d=document.createElement('div');d.className='cap'+(m.partial?' partial':'');d.textContent=m.text;wrap.appendChild(d)}
    if(S.orig&&m.orig){const o=document.createElement('div');o.className='cap orig';o.textContent=m.orig;wrap.appendChild(o)}
  }
  if(S.hide>0&&!demo)timer=setTimeout(()=>wrap.querySelectorAll('.cap').forEach(c=>c.classList.add('gone')),left);
}
function onMsg(m){
  if(m.style){Object.assign(S,m.style);applyStyle();render(q.has('demo')?DEMO:last);return}
  last=m; if(!q.has('demo'))render(m);
}
applyStyle();
if(q.has('demo'))render(DEMO);
function connect(){const es=new EventSource('/events');es.onmessage=e=>onMsg(JSON.parse(e.data));es.onerror=()=>{es.close();setTimeout(connect,2000)}}
connect();
</script></body></html>"##;

#[derive(Default)]
struct Shared {
    clients: Vec<mpsc::Sender<String>>,
    last: Option<String>,
    style: Option<String>,
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

    /// The caption style (size, position, names...) for every open browser source, now and later.
    pub fn set_style(&self, style: &Value) {
        let data = serde_json::json!({"style": style}).to_string();
        let mut sh = self.shared.lock().unwrap();
        sh.style = Some(data.clone());
        sh.clients.retain(|c| c.send(data.clone()).is_ok());
    }

    /// {text, orig, partial} to every open browser source.
    pub fn send(&self, msg: &Value) {
        let mut msg = msg.clone();
        if let Some(o) = msg.as_object_mut() {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
            o.insert("at".into(), now.into());
        }
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
        let (style, last) = {
            let mut s = sh.lock().unwrap();
            s.clients.push(tx);
            (s.style.clone(), s.last.clone())
        };
        if let Some(st) = style {
            conn.write_all(format!("data: {st}\n\n").as_bytes())?;
        }
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
