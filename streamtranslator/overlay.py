"""Where live captions can go besides the app:

- CaptionOverlay: a borderless always-on-top caption bar you can drag over any
  player (or capture in OBS as a window).
- ObsServer: a tiny local web server. Add http://127.0.0.1:<port>/ as an OBS
  Browser Source and captions appear on your own stream.
"""
from __future__ import annotations

import json
import queue
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from PySide6.QtCore import QPoint, QRectF, Qt, QTimer
from PySide6.QtGui import QColor, QFont, QFontMetrics, QPainter, QPen
from PySide6.QtWidgets import QWidget


class CaptionOverlay(QWidget):
    def __init__(self, scale: float = 1.0):
        super().__init__(None, Qt.FramelessWindowHint | Qt.WindowStaysOnTopHint | Qt.Tool)
        self.scale = max(1.0, scale)   # match the app's interface size on big screens
        self.setAttribute(Qt.WA_TranslucentBackground)
        self.setWindowTitle("StreamTranslator captions")
        self.text = "Captions will appear here"
        self.orig = ""
        self.partial = False
        self.font_px = 30
        self.bg_alpha = 0.55
        self.show_orig = True
        self._drag: QPoint | None = None
        self.resize(int(900 * self.scale), int(140 * self.scale))
        self._fade = QTimer(self, singleShot=True, interval=12000, timeout=self._clear)

    def set_style(self, font_px=None, bg_alpha=None, show_orig=None):
        if font_px is not None:
            self.font_px = int(max(14, min(80, font_px)))
        if bg_alpha is not None:
            self.bg_alpha = float(max(0, min(1, bg_alpha)))
        if show_orig is not None:
            self.show_orig = bool(show_orig)
        self.update()

    def show_caption(self, text: str, orig: str = "", partial: bool = False):
        if partial and not text:
            return
        self.text, self.orig, self.partial = text, orig, partial
        self._fade.start()
        self.update()

    def _clear(self):
        self.text, self.orig = "", ""
        self.update()

    def paintEvent(self, _):
        p = QPainter(self)
        p.setRenderHint(QPainter.Antialiasing)
        r = QRectF(self.rect()).adjusted(2, 2, -2, -2)
        if self.text or self.underMouse():
            p.setPen(Qt.NoPen)
            p.setBrush(QColor(8, 8, 12, int(255 * self.bg_alpha)))
            p.drawRoundedRect(r, 14, 14)
        if self.underMouse():
            p.setPen(QPen(QColor(145, 70, 255, 200), 1.5))
            p.setBrush(Qt.NoBrush)
            p.drawRoundedRect(r, 14, 14)
            p.setPen(QColor(200, 200, 210))
            hint = QFont("Segoe UI")
            hint.setPixelSize(int(12 * self.scale))
            p.setFont(hint)
            p.drawText(r.adjusted(12, 6, -12, -6), Qt.AlignTop | Qt.AlignRight,
                       "drag to move · scroll: text size · ctrl+scroll: width · right-click: hide")
        px = int(self.font_px * self.scale)
        main = QFont("Segoe UI", 10, QFont.Bold)
        main.setPixelSize(px)
        sub = QFont("Segoe UI", 10)
        sub.setPixelSize(max(12, int(px * 0.55)))
        inner = r.adjusted(20, 12, -20, -12)
        lines = []
        if self.text:
            lines.append((self.text, main, QColor(255, 255, 255, 170 if self.partial else 255)))
        if self.orig and self.show_orig:
            lines.append((self.orig, sub, QColor(190, 190, 205, 220)))
        heights = [QFontMetrics(f).boundingRect(inner.toRect(), Qt.TextWordWrap | Qt.AlignHCenter, t).height()
                   for t, f, _ in lines]
        y = inner.bottom() - sum(heights) - 6 * (len(lines) - 1)
        for (t, f, c), h in zip(lines, heights):
            box = QRectF(inner.left(), y, inner.width(), h)
            p.setFont(f)
            p.setPen(QColor(0, 0, 0, 200))   # outline so text reads on any background
            for dx, dy in ((-1.5, 0), (1.5, 0), (0, -1.5), (0, 1.5)):
                p.drawText(box.translated(dx, dy), Qt.TextWordWrap | Qt.AlignHCenter, t)
            p.setPen(c)
            p.drawText(box, Qt.TextWordWrap | Qt.AlignHCenter, t)
            y += h + 6

    def enterEvent(self, e):
        self.update()

    def leaveEvent(self, e):
        self.update()

    def mousePressEvent(self, e):
        if e.button() == Qt.LeftButton:
            self._drag = e.globalPosition().toPoint() - self.frameGeometry().topLeft()
        elif e.button() == Qt.RightButton:
            self.hide()

    def mouseMoveEvent(self, e):
        if self._drag is not None:
            self.move(e.globalPosition().toPoint() - self._drag)
        else:
            self.setCursor(Qt.SizeAllCursor)

    def mouseReleaseEvent(self, e):
        self._drag = None

    def wheelEvent(self, e):
        step = 2 if e.angleDelta().y() > 0 else -2
        if e.modifiers() & Qt.ControlModifier:
            self.resize(max(300, self.width() + int(step * 20 * self.scale)), self.height())
        else:
            self.set_style(font_px=self.font_px + step)
            self.resize(self.width(), max(80, int(self.font_px * self.scale * 3.6)))


_OBS_PAGE = """<!doctype html><html><head><meta charset="utf-8"><title>StreamTranslator captions</title>
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
</script></body></html>"""


class ObsServer:
    def __init__(self, port: int = 47851):
        self.clients: list[queue.Queue] = []
        self.lock = threading.Lock()
        self.last: dict | None = None
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_GET(self):
                if self.path.startswith("/events"):
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Cache-Control", "no-cache")
                    self.end_headers()
                    q: queue.Queue = queue.Queue()
                    with outer.lock:
                        outer.clients.append(q)
                    try:
                        if outer.last:
                            self.wfile.write(f"data: {json.dumps(outer.last)}\n\n".encode())
                            self.wfile.flush()
                        while True:
                            try:
                                msg = q.get(timeout=15)
                                self.wfile.write(f"data: {json.dumps(msg)}\n\n".encode())
                            except queue.Empty:
                                self.wfile.write(b": ping\n\n")
                            self.wfile.flush()
                    except OSError:
                        pass
                    finally:
                        with outer.lock:
                            if q in outer.clients:
                                outer.clients.remove(q)
                    return
                body = _OBS_PAGE.encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        last_err = None
        for p in range(port, port + 10):
            try:
                self.httpd = ThreadingHTTPServer(("127.0.0.1", p), Handler)
                self.port = p
                break
            except OSError as e:
                last_err = e
        else:
            raise last_err
        self.httpd.daemon_threads = True
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self.port}/"

    def send(self, msg: dict):
        self.last = msg
        with self.lock:
            for q in self.clients:
                q.put(msg)

    def close(self):
        self.httpd.shutdown()
