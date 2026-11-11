"""App shell: a Qt window hosting the web UI (ui/), plus the Python backend it talks to.

The page calls `backend.request(json)`; every call gets a `reply` event back.
Long work (downloads, Whisper, live mode, the job queue) reports through events.
"""
from __future__ import annotations

import base64
import json
import os
import subprocess
import sys
import threading
import time
import traceback
import uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from PySide6.QtCore import QObject, Qt, QTimer, QUrl, Signal, Slot
from PySide6.QtGui import (QAction, QColor, QDesktopServices, QGuiApplication, QIcon, QKeySequence, QPainter,
                           QPixmap, QShortcut)
from PySide6.QtWebChannel import QWebChannel
from PySide6.QtWebEngineCore import QWebEnginePage, QWebEngineSettings
from PySide6.QtWebEngineWidgets import QWebEngineView
from PySide6.QtWidgets import QApplication, QFileDialog, QMainWindow, QMenu, QSystemTrayIcon

from . import APP_NAME, __version__, appupdate, history, logs, updater
from .glossary import DATA_DIR, Glossary
from .pipeline import (NO_WINDOW, Callbacks, Cancelled, JobError, JobParams, ffmpeg_exe, fetch_info, grab_frame,
                       lang_name, load_model, model_device, model_is_downloaded, read_meta, read_srt, reburn,
                       run_job, write_meta, write_srt_cues)

UI_DIR = Path(getattr(sys, "_MEIPASS", Path(__file__).resolve().parent.parent)) / "streamtranslator" / "ui" \
    if getattr(sys, "frozen", False) else Path(__file__).resolve().parent / "ui"
SETTINGS_FILE = DATA_DIR / "settings.json"
DEFAULTS = {
    "my_channel": "", "out_dir": str(Path.home() / "Videos" / APP_NAME), "model": "large-v3",
    "live_model": "large-v3", "preload": True, "quality": "720p", "position": "Above game HUD",
    "size": "Medium", "burn": True, "orig": False, "language": "", "use_glossary": True, "isolate": False,
    "live_language": "", "live_orig": True, "live_isolate": False, "live_chat": True,
    "overlay_font": 30, "overlay_bg": 0.55, "ui_scale": "auto", "notify": True, "auto_update": True,
    "method": "two-step", "translator": "better", "auto_details": True, "live_method": "two-step",
    "custom_model": "", "setup_done": False, "app_update": True, "speakers": False,
    "last_version": "", "updated_from": "", "skip_version": "", "app_update": True,
}
# Methods that touch Qt widgets must run on the UI thread.
MAIN_THREAD = {"components.model_folder", "os.browse", "os.copy", "os.clipboard", "overlay.show", "overlay.style", "obs.url", "ui.scale"}


def auto_scale() -> float:
    """Pick an interface size from the screen: 1080p -> 100%, 1440p -> 120%,
    4K at 100% Windows scaling -> 180%. Windows scaling is already included in
    the logical height, so 4K at 150% lands near 120%."""
    screen = QGuiApplication.primaryScreen()
    hgt = screen.availableGeometry().height() if screen else 1080
    return round(max(1.0, min(2.5, hgt / 1200)) * 20) / 20


def load_settings() -> dict:
    try:
        return {**DEFAULTS, **json.loads(SETTINGS_FILE.read_text(encoding="utf-8"))}
    except (OSError, ValueError):
        return dict(DEFAULTS)


def file_url(path) -> str | None:
    return QUrl.fromLocalFile(str(path)).toString() if path else None


class Backend(QObject):
    event = Signal(str)      # to the page (always emitted on the UI thread)
    _out = Signal(str)       # from any thread -> queued to the UI thread
    _notify = Signal(str, str)
    _quit = Signal()

    def __init__(self, window: "MainWindow"):
        super().__init__()
        self.window = window
        self.settings = load_settings()
        self.pool = ThreadPoolExecutor(max_workers=8)
        self.frames = ThreadPoolExecutor(max_workers=3)
        self.vods = {}
        self.live = None
        self.chat = None
        self.overlay = None
        self.obs = None
        self.gpu = None
        self.update_info: dict = {"current": updater.active_version()}
        # job queue
        self.jobs: list[dict] = []
        self.qcond = threading.Condition()
        self.cancel_current: threading.Event | None = None
        threading.Thread(target=self._queue_worker, daemon=True).start()
        self.snap_cancel: threading.Event | None = None
        self._out.connect(self._deliver, Qt.QueuedConnection)
        self._notify.connect(self._show_notification, Qt.QueuedConnection)
        self._quit.connect(self._quit_for_update, Qt.QueuedConnection)
        self.app_update: dict = {"current": __version__}
        self.updating = False

    # ---- plumbing
    def emit(self, type_: str, **data):
        self._out.emit(json.dumps({"type": type_, **data}, default=str))

    @Slot(str)
    def _deliver(self, raw: str):
        self.event.emit(raw)
        self._route(raw)   # mirror live captions into the overlay / OBS

    @Slot(str)
    def request(self, raw: str):
        msg = json.loads(raw)
        rid, method, params = msg.get("id"), msg.get("method", ""), msg.get("params") or {}
        fn = getattr(self, "m_" + method.replace(".", "_"), None)
        if fn is None:
            self.emit("reply", id=rid, ok=False, error=f"unknown method {method}")
            return

        def run():
            try:
                self.emit("reply", id=rid, ok=True, result=fn(**params))
            except (JobError, ValueError, Cancelled) as e:
                self.emit("reply", id=rid, ok=False, error=str(e) or "Cancelled")
            except Exception as e:
                logs.log.exception(f"{method} failed")
                self.emit("reply", id=rid, ok=False, error=f"{type(e).__name__}: {e}")
        if method in MAIN_THREAD:
            run()
        else:
            (self.frames if method == "vod.frame" else self.pool).submit(run)

    def _route(self, raw: str):
        if not (self.overlay or self.obs) or '"live.' not in raw[:20]:
            return
        m = json.loads(raw)
        if m["type"] == "live.caption":
            payload = {"text": m["text"], "orig": m.get("orig", ""), "partial": False}
        elif m["type"] == "live.partial":
            payload = {"text": m["text"], "orig": "", "partial": True}
        else:
            return
        if self.overlay and self.overlay.isVisible():
            self.overlay.show_caption(payload["text"], payload["orig"], payload["partial"])
        if self.obs:
            self.obs.send(payload)

    # ---- notifications
    def notify(self, title: str, text: str):
        if self.settings.get("notify", True):
            self._notify.emit(title, text)

    @Slot(str, str)
    def _show_notification(self, title: str, text: str):
        w = self.window
        if w.isActiveWindow() and not w.isMinimized():
            return                    # they're looking at the app already
        if w.tray and w.tray.isVisible():
            w.tray.showMessage(title, text, w.windowIcon(), 8000)
        QApplication.alert(w)

    # ---- app
    def m_app_init(self):
        gl = Glossary.get()
        if self.gpu is None:
            try:
                r = subprocess.run(["nvidia-smi", "--query-gpu=name", "--format=csv,noheader"],
                                   capture_output=True, text=True, timeout=5, creationflags=NO_WINDOW)
                self.gpu = r.stdout.strip().splitlines()[0] if r.returncode == 0 and r.stdout.strip() else ""
            except Exception:
                self.gpu = ""
        threading.Thread(target=self._refresh_champions, daemon=True).start()
        if self.settings.get("auto_update", True):
            threading.Thread(target=self._check_update, daemon=True).start()
        if self.settings.get("app_update", True):
            threading.Thread(target=self.m_appupdate_check, daemon=True).start()
        from . import components
        from .pipeline import MODEL_PATHS
        if self.settings.get("custom_model"):
            MODEL_PATHS["custom"] = self.settings["custom_model"]
        comp = self.m_components_status()
        gpu_ready = comp["gpu"]["ok"] or not comp["has_nvidia"]
        model = self.settings.get("model", "large-v3")
        setup_needed = (not gpu_ready or not comp["ffmpeg"]["ok"]
                        or not (model == "custom" or comp["models"]["whisper"].get(model, {}).get("ok")))
        if self.settings.get("preload") and gpu_ready and not setup_needed:
            self.pool.submit(self._preload, model)
        try:
            ffmpeg_ok = bool(ffmpeg_exe())
        except JobError:
            ffmpeg_ok = False
        return {"settings": self.settings, "glossary": gl.summary(), "gpu": self.gpu, "version": __version__,
                "scale": {"value": self.window.view.zoomFactor(), "auto": auto_scale()},
                "update": self.update_info, "app_update": self.app_update, "whats_new": self._whats_new(),
                "crashed": logs.previous_crash, "ffmpeg": ffmpeg_ok, "queue": self._queue_view(),
                "components": comp, "setup_needed": setup_needed,
                "model": {"name": self.settings.get("model"),
                          "device": model_device(self.settings.get("model", "large-v3"))}}

    def _refresh_champions(self):
        try:
            if Glossary.get().refresh_champions():
                self.emit("glossary.updated", glossary=Glossary.get().summary())
        except Exception:
            pass   # offline: the cached or built-in list still works

    def _check_update(self):
        try:
            r = updater.check_and_update()
            self.update_info = {**r, "checked": time.time()}
        except Exception as e:
            self.update_info = {"current": updater.active_version(), "error": str(e), "checked": time.time()}
        self.emit("update.status", **self.update_info)
        return self.update_info

    def m_update_check(self):
        return self._check_update()

    # ---- app updates (GitHub releases)
    def m_appupdate_check(self):
        try:
            self.app_update = {**appupdate.check(), "checked": time.time()}
        except Exception as e:
            self.app_update = {"current": __version__, "error": str(e), "checked": time.time()}
        self.emit("appupdate.status", **self.app_update)
        return self.app_update

    def m_appupdate_install(self):
        info = self.app_update
        if not info.get("newer") or not info.get("asset"):
            info = self.m_appupdate_check()
        if not info.get("newer"):
            raise ValueError("You already have the newest version.")
        if any(j["status"] in ("pending", "running") for j in self.jobs) or self.live:
            raise ValueError("Finish or stop the running translations first, then update.")
        if self.updating:
            raise ValueError("The update is already downloading.")
        return self._install_release(info, updated_from=__version__)

    def m_appupdate_rollback(self):
        """Go back to the version installed before the last update."""
        prev = self.settings.get("updated_from")
        if not prev or not appupdate.is_newer(__version__, prev):
            raise ValueError("There's no earlier version to go back to.")
        info = appupdate.release_info(prev)
        if not info.get("asset"):
            raise ValueError(f"Version {prev}'s installer isn't on GitHub anymore.")
        logs.log.info(f"Going back from {__version__} to {prev}")
        # don't offer this version again right away (newer versions will still be offered)
        return self._install_release(info, updated_from="", skip=__version__)

    def _install_release(self, info: dict, updated_from: str, skip: str = ""):
        if any(j["status"] in ("pending", "running") for j in self.jobs) or self.live:
            raise ValueError("Finish or stop the running translations first, then update.")
        if self.updating:
            raise ValueError("The update is already downloading.")
        self.updating = True
        try:
            path = appupdate.download(info["asset"], progress=lambda f: self.emit("appupdate.progress", f=f))
            appupdate.save_notes(info["latest"], info.get("notes", ""))
            self.m_settings_save({"updated_from": updated_from, "skip_version": skip})
            logs.log.info(f"Installing {info['latest']} (from {__version__})")
            appupdate.install(path)
        finally:
            self.updating = False
        self.emit("appupdate.progress", f=1, installing=True)
        self._quit.emit()
        return {"installing": info["latest"]}

    def _whats_new(self) -> dict | None:
        """Release notes once, on the first start after an update."""
        last = self.settings.get("last_version") or ""
        if last == __version__:
            return None
        self.m_settings_save({"last_version": __version__})
        if not last or not appupdate.is_newer(__version__, last):
            return None
        return {"version": __version__, "from": last, "notes": appupdate.notes_for(__version__)}

    # ---- problems
    def m_app_report(self, what: str = ""):
        extra = {"gpu": self.gpu or "", "Model": self.settings.get("model"), "Method": self.settings.get("method"),
                 "Translator": self.settings.get("translator")}
        url = logs.report_url(what, extra)
        QDesktopServices.openUrl(QUrl(url))
        return True

    def m_app_logs(self):
        logs.LOG_DIR.mkdir(parents=True, exist_ok=True)
        QDesktopServices.openUrl(QUrl.fromLocalFile(str(logs.LOG_DIR)))
        return True

    @Slot()
    def _quit_for_update(self):
        QTimer.singleShot(600, lambda: (self.window.close(), QApplication.quit()))

    def _preload(self, name: str):
        if model_device(name):
            self.emit("model.status", state="ready", name=name, device=model_device(name))
            return
        first = not model_is_downloaded(name)
        self.emit("model.status", state="downloading" if first else "loading", name=name)
        try:
            load_model(name)
            self.emit("model.status", state="ready", name=name, device=model_device(name))
        except Exception as e:
            self.emit("model.status", state="error", name=name, error=str(e))

    # ---- components (first-run setup)
    def m_components_status(self):
        from . import components
        st = components.status()
        st["custom_model"] = self.settings.get("custom_model") or ""
        st["selected_model"] = self.settings.get("model", "large-v3")
        st["translator"] = self.settings.get("translator", "better")
        return st

    def m_components_download(self, what: str, name: str = ""):
        """what: gpu | whisper | translator | isolation. Runs in the background with progress events."""
        from . import components, cuda

        def work():
            prog = lambda f, s: self.emit("comp.progress", what=what, name=name, f=round(f, 3), text=s)
            try:
                if what == "gpu":
                    components.download_gpu_libs(prog)
                    cuda.refresh()
                else:
                    components.download_model(what, name, prog)
                self.emit("comp.done", what=what, name=name, status=self.m_components_status())
            except Exception as e:
                self.emit("comp.error", what=what, name=name, message=str(e))
        threading.Thread(target=work, daemon=True).start()
        return True

    def m_components_use_gpu_dir(self, dir: str, label: str):
        from . import components, cuda
        components.use_existing(dir, label)
        cuda.refresh()
        return self.m_components_status()

    def m_components_model_folder(self):
        from . import components
        from .pipeline import MODEL_PATHS
        d = QFileDialog.getExistingDirectory(self.window, "Pick a faster-whisper model folder", str(Path.home()))
        if not d:
            return None
        path = components.check_model_folder(d)
        MODEL_PATHS["custom"] = path
        self.m_settings_save({"custom_model": path, "model": "custom"})
        return self.m_components_status()

    def m_setup_done(self):
        self.m_settings_save({"setup_done": True})
        return True

    def m_model_preload(self, name: str):
        self._preload(name)
        return True

    def m_settings_save(self, data: dict):
        self.settings.update({k: v for k, v in data.items() if k in DEFAULTS})
        DATA_DIR.mkdir(parents=True, exist_ok=True)
        SETTINGS_FILE.write_text(json.dumps(self.settings, indent=2), encoding="utf-8")
        return self.settings

    # ---- sources
    def _info(self, text: str):
        info = fetch_info(text)
        self.vods[info.id] = info
        return info

    def m_source_load(self, text: str):
        from .sync import classify, list_vods
        kind, value = classify(text)
        if kind == "channel":
            return {"kind": "channel", "login": value, "vods": list_vods(value)}
        return {"kind": "vod", "vod": self._info(value).to_dict()}

    def m_vod_frame(self, id: str, t: float, w: int = 480):
        info = self.vods.get(id)
        if not info:
            raise JobError("VOD not loaded")
        data = grab_frame(info, t, width=w)
        return "data:image/jpeg;base64," + base64.b64encode(data).decode()

    def m_sync_match(self, mine: str, my_start: float, my_end: float, theirs):
        """theirs: one channel/VOD or a list of them. Returns one result per entry."""
        from .sync import match
        my_info = self._info(mine)
        targets = [theirs] if isinstance(theirs, str) else list(theirs)

        def one(t):
            try:
                r = match(my_info, my_start, my_end, t)
                self.vods[r["theirs"].id] = r["theirs"]
                return {"ok": True, "input": t, "theirs": r["theirs"].to_dict(), "start": r["start"],
                        "end": r["end"], "offset": r["offset"], "clipped": r["clipped"]}
            except Exception as e:
                return {"ok": False, "input": t, "error": str(e)}
        with ThreadPoolExecutor(max_workers=4) as ex:
            results = list(ex.map(one, targets))
        return {"mine": my_info.to_dict(), "results": results}

    def m_snap_run(self, id: str, lo: float, hi: float):
        from .snap import find_game
        info = self.vods.get(id)
        if not info:
            raise JobError("VOD not loaded")
        self.snap_cancel = threading.Event()
        return find_game(info, lo, hi, lambda f, s: self.emit("snap.progress", f=round(f, 3), text=s), self.snap_cancel)

    def m_snap_cancel(self):
        if self.snap_cancel:
            self.snap_cancel.set()
        return True

    # ---- job queue (clips and re-burns run one at a time)
    def _queue_view(self) -> list[dict]:
        with self.qcond:
            return [{k: j.get(k) for k in ("id", "kind", "label", "status", "error", "result", "vod_id", "start")}
                    for j in self.jobs]

    def _queue_changed(self):
        self.emit("queue.update", jobs=self._queue_view())

    def m_queue_add(self, jobs: list[dict]):
        ids = []
        with self.qcond:
            for p in jobs:
                kind = p.pop("kind", "clip")
                jid = uuid.uuid4().hex[:8]
                label = p.get("label") or (f"Re-burn subtitles" if kind == "burn" else "Clip")
                self.jobs.append({"id": jid, "kind": kind, "params": p, "label": label, "status": "pending",
                                  "vod_id": p.get("vod_id"), "start": p.get("start")})
                ids.append(jid)
            self.qcond.notify()
        self._queue_changed()
        return ids

    def m_clip_start(self, **p):
        return self.m_queue_add([p])[0]

    def m_queue_cancel(self, id: str | None = None):
        with self.qcond:
            for j in self.jobs:
                if (id is None or j["id"] == id) and j["status"] in ("pending", "running"):
                    if j["status"] == "running" and self.cancel_current:
                        self.cancel_current.set()
                    else:
                        j["status"] = "cancelled"
        self._queue_changed()
        return True

    def m_clip_cancel(self):
        with self.qcond:
            running = next((j for j in self.jobs if j["status"] == "running"), None)
        return self.m_queue_cancel(running["id"]) if running else True

    def m_queue_clear(self):
        with self.qcond:
            self.jobs = [j for j in self.jobs if j["status"] in ("pending", "running")]
        self._queue_changed()
        return True

    def _queue_worker(self):
        batch_done = 0
        while True:
            with self.qcond:
                while not any(j["status"] == "pending" for j in self.jobs):
                    if batch_done > 1:
                        self.notify("All translations finished", f"{batch_done} jobs are done.")
                    batch_done = 0
                    self.qcond.wait()
                job = next(j for j in self.jobs if j["status"] == "pending")
                job["status"] = "running"
                self.cancel_current = cancel = threading.Event()
            self._queue_changed()
            jid = job["id"]
            cb = Callbacks(
                stage=lambda i, n, label: self.emit("clip.stage", job=jid, i=i, n=n, label=label),
                progress=lambda f: self.emit("clip.progress", job=jid, f=round(f, 4)),
                log=lambda s: (logs.log.info(f"[{job['label']}] {s}"), self.emit("clip.log", job=jid, text=s)),
                segment=lambda seg, kind: self.emit("clip.segment", job=jid, start=seg.start, end=seg.end,
                                                    text=seg.text, kind=kind),
                cancel=cancel)
            logs.log.info(f"Job started: {job['label']} ({job['kind']}) "
                          + str({k: v for k, v in job["params"].items() if k not in ("out_dir", "champions")}))
            try:
                result = self._run_clip(job, cb) if job["kind"] == "clip" else self._run_burn(job, cb)
                logs.log.info(f"Job finished: {job['label']}")
                job.update(status="done", result=result)
                batch_done += 1
                self.emit("clip.done", job=jid, kind=job["kind"], **result)
            except Cancelled:
                job["status"] = "cancelled"
                self.emit("clip.cancelled", job=jid)
            except JobError as e:
                logs.log.warning(f"Job failed: {job['label']}: {e}")
                job.update(status="error", error=str(e))
                self.emit("clip.error", job=jid, message=str(e))
                self.notify("Translation failed", f"{job['label']}: {str(e).splitlines()[0]}")
            except Exception as e:
                logs.log.exception(f"Job crashed: {job['label']}")
                msg = (f"Something went wrong: {type(e).__name__}: {e}\n\n"
                       "The details are in the log. Use Report to send them.")
                job.update(status="error", error=msg)
                self.emit("clip.error", job=jid, message=msg, report=True)
                self.notify("Translation failed", f"{job['label']}: {type(e).__name__}")
            self._queue_changed()

    def _run_clip(self, job: dict, cb: Callbacks) -> dict:
        p = job["params"]
        params = JobParams(url=p["url"], start=float(p["start"]), end=float(p["end"]),
                           out_dir=Path(p.get("out_dir") or self.settings["out_dir"]),
                           quality=p.get("quality", "720p"), model=p.get("model", "large-v3"),
                           language=p.get("language") or None, want_original=bool(p.get("orig")),
                           burn=bool(p.get("burn", True)), position=p.get("position", "Above game HUD"),
                           font_size=p.get("size", "Medium"), champions=p.get("champions") or [],
                           use_glossary=bool(p.get("use_glossary", True)), isolate=bool(p.get("isolate")),
                           label=job["label"], method=p.get("method", "two-step"),
                           translator=self.settings.get("translator", "better"),
                           auto_details=bool(p.get("auto_details", True)), speakers=bool(p.get("speakers")))
        r = run_job(params, cb)
        self.emit("model.status", state="ready", name=params.model, device=model_device(params.model))
        m = r.meta
        entry = history.add({
            "type": "clip", "folder": str(r.folder), "title": m["title"], "channel": m["channel"],
            "vod_url": m["vod_url"], "vod_id": m["vod_id"], "start": m["start"], "end": m["end"],
            "language": r.language, "language_name": lang_name(r.language), "lines": r.segments,
            "thumb": str(r.folder / m["thumb"]) if m.get("thumb") else None,
            "clip": str(r.clip), "srt": str(r.srt), "transcript": str(r.transcript),
            "subbed": str(r.subbed) if r.subbed else None, "seconds": r.seconds})
        self.emit("history.changed")
        self.notify("Translation finished", f"{m['channel']}: {r.segments} lines from {lang_name(r.language)}")
        return {"folder": str(r.folder), "clip": str(r.clip), "srt": str(r.srt), "transcript": str(r.transcript),
                "subbed": str(r.subbed) if r.subbed else None, "language": r.language,
                "language_name": lang_name(r.language), "probability": r.probability, "segments": r.segments,
                "seconds": r.seconds, "history_id": entry["id"], "channel": m["channel"]}

    def _run_burn(self, job: dict, cb: Callbacks) -> dict:
        p = job["params"]
        t0 = time.time()
        out = reburn(Path(p["folder"]), p.get("position", "Above game HUD"), p.get("size", "Medium"), cb)
        for e in history.listing():
            if e.get("folder") == p["folder"]:
                history.update(e["id"], subbed=str(out))
        self.emit("history.changed")
        self.notify("Subtitles burned", "The edited video is ready.")
        return {"folder": p["folder"], "subbed": str(out), "seconds": time.time() - t0}

    # ---- subtitle editor
    def m_subs_open(self, folder: str, h264: bool = False, av1: bool = False):
        folder_p = Path(folder)
        meta = read_meta(folder_p)
        cues = read_srt(folder_p / meta["srt"])
        clip = folder_p / meta["clip"]
        if not clip.exists():
            raise JobError("The clip file for these subtitles is missing.")
        audio_only = bool(meta.get("audio_only"))
        video = clip if (h264 and not audio_only) else folder_p / "preview.webm"
        if not video.exists():
            self._make_preview(clip, video, av1, max(1.0, meta["end"] - meta["start"]), audio_only)
        thumb = folder_p / meta["thumb"] if meta.get("thumb") else None
        return {"cues": cues, "video": file_url(video), "meta": meta, "folder": folder,
                "poster": file_url(thumb) if thumb and thumb.exists() else None}

    def _make_preview(self, clip: Path, out: Path, av1: bool, dur: float, audio_only: bool = False):
        """This browser engine can't play H.264, so make a quick WebM copy for the editor."""
        from .pipeline import run_ffmpeg
        cancel = threading.Event()
        prog = lambda f: self.emit("subs.progress", f=round(f, 3))
        tmp = out.with_name("preview.part.webm")
        tries = []
        if audio_only:      # nothing to show: just a small audio track for the editor
            tries.append(["-c:a", "libopus"])
        elif av1:
            tries.append(["-c:v", "av1_nvenc", "-preset", "p2", "-cq", "38"])
        tries.append(["-c:v", "libvpx", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "1500k", "-threads", "8"])
        last = None
        for enc in tries:
            try:
                video = ["-vn"] if audio_only else ["-vf", "scale=-2:540"]
                run_ffmpeg(["-i", str(clip), *video, *enc, "-c:a", "libopus", "-b:a", "96k",
                            str(tmp)], dur, prog, cancel)
                tmp.replace(out)
                return
            except JobError as e:
                last = e
        raise JobError(f"Couldn't make the preview video: {last}")

    def m_subs_save(self, folder: str, cues: list[dict]):
        folder_p = Path(folder)
        meta = read_meta(folder_p)
        srt = folder_p / meta["srt"]
        backup = srt.with_name(srt.stem + ".original.srt")
        if not backup.exists() and srt.exists():
            backup.write_bytes(srt.read_bytes())
        write_srt_cues(cues, srt)
        meta.update(edited=time.time(), lines=len([c for c in cues if c.get("text", "").strip()]))
        write_meta(folder_p, meta)
        for e in history.listing():
            if e.get("folder") == folder:
                history.update(e["id"], lines=meta["lines"], edited=meta["edited"])
        self.emit("history.changed")
        return {"saved": str(srt), "backup": str(backup)}

    def m_subs_burn(self, folder: str, position: str, size: str):
        return self.m_queue_add([{"kind": "burn", "folder": folder, "position": position, "size": size,
                                  "label": "Re-burn edited subtitles"}])[0]

    # ---- history
    def m_history_list(self):
        items = history.listing()
        for e in items:
            e["thumb_url"] = file_url(e["thumb"]) if e.get("thumb") and Path(e["thumb"]).exists() else None
        return items

    def m_history_search(self, q: str):
        return history.search(q)

    def m_history_remove(self, id: str):
        history.remove(id)
        return True

    # ---- live
    def m_live_start(self, **p):
        from .live import LiveSession
        from .sync import classify
        self.m_live_stop()
        self.live = live = LiveSession(source=p["source"], model=p.get("model", "large-v3"),
                                       language=p.get("language") or None, show_original=bool(p.get("orig", True)),
                                       use_glossary=bool(p.get("use_glossary", True)),
                                       champions=p.get("champions") or [], save_dir=Path(self.settings["out_dir"]),
                                       emit=self._live_emit, isolate=bool(p.get("isolate")),
                                       method=p.get("method", "two-step"),
                                       translator=self.settings.get("translator", "better"))
        live.start()
        if p.get("chat", True):
            kind, value = classify(p["source"])
            if kind == "channel":
                self._start_chat(value)
            else:
                self.emit("chat.status", state="unavailable", message="Chat translation works for Twitch channels.")
        return True

    def _live_emit(self, type_: str, **data):
        self.emit(type_, **data)
        live = self.live
        if type_ == "live.status" and data.get("state") == "stopped" and live and live.transcript_path:
            if live.lines:
                history.add({"type": "live", "channel": live.info.get("channel", ""), "title": live.info.get("title", ""),
                             "url": live.info.get("url", ""), "transcript": str(live.transcript_path),
                             "srt": str(live._srt_path) if live._srt_path else None,
                             "thumb": str(live.thumb_path) if live.thumb_path else None, "lines": live.lines,
                             "duration": time.time() - live.started})
                self.emit("history.changed")
        if type_ == "live.error" and data.get("fatal", True) is not False:
            self.notify("Live translation stopped", data.get("message", ""))

    def _start_chat(self, channel: str):
        from .chat import ChatSession
        if self.chat:
            self.chat.stop()
        live = self.live
        self.chat = ChatSession(channel, lambda: (live.locked_lang or live.language) if live else None, self.emit,
                                size=self.settings.get("translator", "better"))
        self.chat.start()

    def m_chat_toggle(self, on: bool):
        from .sync import classify
        if not on:
            if self.chat:
                self.chat.stop()
                self.chat = None
            self.emit("chat.status", state="off")
            return True
        if self.live and not self.live.stop_event.is_set():
            kind, value = classify(self.live.source)
            if kind == "channel":
                self._start_chat(value)
        return True

    def m_live_stop(self):
        if self.live:
            self.live.stop()
        if self.chat:
            self.chat.stop()
            self.chat = None
        return True

    def m_live_update(self, **p):
        if self.live:
            self.live.update(language=(p["language"] or None) if "language" in p else ...,
                             show_original=p.get("orig"), champions=p.get("champions"),
                             use_glossary=p.get("use_glossary"), isolate=p.get("isolate"),
                             method=p.get("method"))
        return True

    def m_overlay_show(self, show: bool):
        from .overlay import CaptionOverlay
        if show:
            if not self.overlay:
                self.overlay = CaptionOverlay(scale=self.window.view.zoomFactor())
                self.overlay.set_style(font_px=self.settings["overlay_font"], bg_alpha=self.settings["overlay_bg"])
                geo = QGuiApplication.primaryScreen().availableGeometry()
                self.overlay.move(geo.center().x() - self.overlay.width() // 2, geo.bottom() - self.overlay.height() - 80)
            self.overlay.show()
            self.overlay.raise_()
        elif self.overlay:
            self.overlay.hide()
        return bool(self.overlay and self.overlay.isVisible())

    def m_overlay_style(self, font: int | None = None, bg: float | None = None, orig: bool | None = None):
        if self.overlay:
            self.overlay.set_style(font_px=font, bg_alpha=bg, show_orig=orig)
        return True

    def m_ui_scale(self, value):
        """value: 'auto', a number like 1.5, or '+' / '-' / '0' from the keyboard shortcuts."""
        cur = self.window.view.zoomFactor()
        if value == "+":
            value = min(3.0, round((cur + 0.1) * 20) / 20)
        elif value == "-":
            value = max(0.5, round((cur - 0.1) * 20) / 20)
        elif value == "0":
            value = "auto"
        self.settings["ui_scale"] = value
        self.m_settings_save({})
        z = self.window.apply_scale()
        self.emit("ui.scale", value=z, setting=value, auto=auto_scale())
        return z

    def m_obs_url(self):
        from .overlay import ObsServer
        if not self.obs:
            self.obs = ObsServer()
        return self.obs.url

    # ---- glossary
    def m_glossary_get(self):
        return Glossary.get().summary()

    def m_glossary_save(self, data: dict):
        Glossary.get().update(data)
        return Glossary.get().summary()

    def m_glossary_test(self, text: str):
        fixed, n = Glossary.get().fix(text, english=True)
        return {"fixed": fixed, "changes": n}

    def m_glossary_slang_test(self, text: str, lang: str):
        from . import translate
        if not translate.supported(lang):
            raise ValueError("The text translator doesn't cover that language.")
        gl = Glossary.get()
        helped, n = gl.prepare(text, lang)
        tr = translate.Translator.get(self.settings.get("translator", "better"))
        plain, out = tr.translate([text, helped], lang)
        return {"plain": gl.fix(plain)[0], "helped": gl.fix(out)[0], "changes": n}

    def m_glossary_refresh(self):
        Glossary.get().refresh_champions(force=True)
        return Glossary.get().summary()

    # ---- OS helpers
    def m_os_open(self, path: str):
        if path and Path(path).exists():
            os.startfile(path)
        return True

    def m_os_reveal(self, path: str):
        if path and Path(path).exists():
            subprocess.Popen(["explorer", "/select,", str(Path(path))], creationflags=NO_WINDOW)
        return True

    def m_os_url(self, url: str):
        QDesktopServices.openUrl(QUrl(url))
        return True

    def m_os_browse(self, current: str = ""):
        d = QFileDialog.getExistingDirectory(self.window, "Save translated clips to", current or str(Path.home()))
        return d or None

    def m_os_copy(self, text: str):
        QGuiApplication.clipboard().setText(text)
        return True

    def m_os_clipboard(self):
        return QGuiApplication.clipboard().text()

    def shutdown(self):
        self.m_queue_cancel()
        self.m_live_stop()
        if self.overlay:
            self.overlay.close()
        if self.obs:
            self.obs.close()


class Page(QWebEnginePage):
    """Open normal links in the real browser instead of inside the app."""
    def acceptNavigationRequest(self, url, nav_type, is_main):
        if url.scheme() in ("http", "https") and nav_type == QWebEnginePage.NavigationTypeLinkClicked:
            QDesktopServices.openUrl(url)
            return False
        return super().acceptNavigationRequest(url, nav_type, is_main)

    def javaScriptConsoleMessage(self, level, message, line, source):
        print(f"[js:{line}] {message}", file=sys.stderr)


class MainWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle(APP_NAME)
        self.setWindowIcon(_icon())
        self.view = QWebEngineView(self)
        self.page = Page(self.view)
        self.page.setBackgroundColor(QColor("#09090d"))
        self.view.setPage(self.page)
        s = self.page.settings()
        s.setAttribute(QWebEngineSettings.LocalContentCanAccessRemoteUrls, True)
        s.setAttribute(QWebEngineSettings.LocalContentCanAccessFileUrls, True)
        s.setAttribute(QWebEngineSettings.PlaybackRequiresUserGesture, False)
        self.tray = None
        self.backend = Backend(self)
        self.channel = QWebChannel(self.page)
        self.channel.registerObject("backend", self.backend)
        self.page.setWebChannel(self.channel)
        self.view.loadFinished.connect(lambda _ok: self.apply_scale())
        self.view.load(QUrl.fromLocalFile(str(UI_DIR / "index.html")))
        self.setCentralWidget(self.view)
        self._fit_to_screen()
        self._make_tray()
        for keys, val in ((("Ctrl+=", "Ctrl++"), "+"), (("Ctrl+-",), "-"), (("Ctrl+0",), "0")):
            for k in keys:
                QShortcut(QKeySequence(k), self, activated=lambda v=val: self.backend.m_ui_scale(v),
                          context=Qt.ApplicationShortcut)
        _dark_titlebar(self)

    def _make_tray(self):
        if not QSystemTrayIcon.isSystemTrayAvailable():
            return
        self.tray = QSystemTrayIcon(self.windowIcon(), self)
        self.tray.setToolTip(APP_NAME)
        menu = QMenu()
        show = QAction("Open StreamTranslator", menu, triggered=self.bring_front)
        quit_ = QAction("Quit", menu, triggered=QApplication.quit)
        menu.addAction(show)
        menu.addAction(quit_)
        self.tray.setContextMenu(menu)
        self.tray.activated.connect(lambda reason: self.bring_front()
                                    if reason == QSystemTrayIcon.Trigger else None)
        self.tray.messageClicked.connect(self.bring_front)
        self.tray.show()

    def bring_front(self):
        self.showNormal() if self.isMinimized() else self.show()
        self.raise_()
        self.activateWindow()

    def apply_scale(self) -> float:
        s = self.backend.settings.get("ui_scale", "auto")
        try:
            z = float(s)
        except (TypeError, ValueError):
            z = auto_scale()
        self.view.setZoomFactor(z)
        return z

    def _fit_to_screen(self):
        """Open at ~80% of the screen, centred, whatever the resolution."""
        screen = QGuiApplication.primaryScreen()
        if not screen:
            self.resize(1320, 900)
            return
        g = screen.availableGeometry()
        w, hgt = int(g.width() * 0.8), int(g.height() * 0.85)
        self.resize(w, hgt)
        self.move(g.x() + (g.width() - w) // 2, g.y() + (g.height() - hgt) // 2)
        z = auto_scale()
        self.setMinimumSize(int(900 * z), int(620 * z))

    def closeEvent(self, e):
        self.backend.shutdown()
        if self.tray:
            self.tray.hide()
        super().closeEvent(e)


def _dark_titlebar(w: QMainWindow):
    if sys.platform != "win32":
        return
    try:
        import ctypes
        hwnd = int(w.winId())
        on = ctypes.c_int(1)
        ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 20, ctypes.byref(on), 4)      # dark mode
        color = ctypes.c_int(0x000D0909)                                                 # caption #09090d (BGR)
        ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, 35, ctypes.byref(color), 4)
    except Exception:
        pass


def _icon() -> QIcon:
    ico = Path(getattr(sys, "_MEIPASS", Path(__file__).resolve().parent.parent)) / "assets" / "icon.ico"
    if ico.exists():
        return QIcon(str(ico))
    pm = QPixmap(64, 64)
    pm.fill(Qt.transparent)
    p = QPainter(pm)
    p.setRenderHint(QPainter.Antialiasing)
    from PySide6.QtGui import QFont, QLinearGradient
    g = QLinearGradient(0, 0, 64, 64)
    g.setColorAt(0, QColor("#9146FF"))
    g.setColorAt(1, QColor("#22D3EE"))
    p.setPen(Qt.NoPen)
    p.setBrush(g)
    p.drawRoundedRect(4, 4, 56, 56, 16, 16)
    p.setPen(QColor("white"))
    p.setFont(QFont("Segoe UI", 20, QFont.Bold))
    p.drawText(pm.rect(), Qt.AlignCenter, "Aa")
    p.end()
    return QIcon(pm)


def selftest(out_path: str) -> int:
    """Used by build.ps1 to check the packaged app can load its parts."""
    lines = []
    def check(name, fn):
        try:
            lines.append(f"OK   {name}: {fn()}")
        except Exception as e:
            lines.append(f"FAIL {name}: {type(e).__name__}: {e}")
    check("yt-dlp", lambda: __import__("yt_dlp").version.__version__)
    check("faster-whisper", lambda: __import__("faster_whisper").__version__)
    def gpu():
        from . import cuda
        cuda.setup()
        import ctranslate2
        return f"{ctranslate2.get_cuda_device_count()} CUDA device(s)"
    check("ctranslate2", gpu)
    check("onnxruntime", lambda: __import__("onnxruntime").get_available_providers())
    check("windows ocr", lambda: __import__("streamtranslator.snap", fromlist=["x"])._get_engine() is not None)
    check("sentencepiece", lambda: __import__("sentencepiece").__version__)
    check("ui files", lambda: (UI_DIR / "index.html").exists() or (_ for _ in ()).throw(FileNotFoundError(UI_DIR)))
    check("components", lambda: __import__("streamtranslator.components", fromlist=["x"]).status()["ffmpeg"])
    def whisper_gpu():
        from .pipeline import load_model, model_device, model_is_downloaded
        if not model_is_downloaded("large-v3"):
            return "skipped (model not downloaded)"
        load_model("large-v3")
        return f"large-v3 loaded on {model_device('large-v3')}"
    check("whisper", whisper_gpu)
    check("vad model", lambda: __import__("faster_whisper.vad", fromlist=["x"]).get_vad_model() is not None)
    Path(out_path).write_text("\n".join(lines) + "\n", encoding="utf-8")
    return 0


def main() -> int:
    updater.activate()                              # newest downloaded yt-dlp, before anything imports it
    windowed = sys.stdout is None or sys.stderr is None   # no console: library output goes to the log
    if windowed:
        sys.stdout = sys.stdout or open(os.devnull, "w")
        sys.stderr = sys.stderr or open(os.devnull, "w")
    if len(sys.argv) >= 3 and sys.argv[1] == "--selftest":
        return selftest(sys.argv[2])
    logs.setup(windowed)                            # log file, crash capture, "didn't close normally" check
    try:
        (DATA_DIR / "app.log").unlink(missing_ok=True)     # the old unbounded log from 2.2 and earlier
    except OSError:
        pass
    if sys.platform == "win32":
        try:
            import ctypes
            ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID(f"{APP_NAME}.{__version__}")
            # lets the installer notice a running copy and ask to close it
            globals()["_mutex"] = ctypes.windll.kernel32.CreateMutexW(None, False, "StreamTranslatorAppMutex")
        except Exception:
            pass
    os.environ.setdefault("QTWEBENGINE_CHROMIUM_FLAGS", "--disable-gpu-driver-bug-workarounds")
    app = QApplication(sys.argv)
    app.setApplicationName(APP_NAME)
    app.setQuitOnLastWindowClosed(True)
    w = MainWindow()
    app.setWindowIcon(w.windowIcon())
    w.show()
    code = app.exec()
    logs.clean_exit()
    return code


if __name__ == "__main__":
    raise SystemExit(main())
