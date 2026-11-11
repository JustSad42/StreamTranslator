# -*- mode: python ; coding: utf-8 -*-
# PyInstaller recipe for StreamTranslator.exe (one folder). Run through build.ps1.
import os
import site
from pathlib import Path

from PyInstaller.utils.hooks import collect_data_files, collect_dynamic_libs, collect_submodules

sp = Path(site.getsitepackages()[-1]) if (Path(site.getsitepackages()[-1]) / "nvidia").exists() else \
    next(Path(p) for p in site.getsitepackages() if (Path(p) / "nvidia").exists())

# NVIDIA's GPU libraries are NOT bundled (they're ~2 GB): the app's setup screen finds an
# existing copy or downloads them on first run. FFmpeg IS bundled so nothing else is needed.
import shutil
ffmpeg_path = shutil.which("ffmpeg")
if not ffmpeg_path:
    raise SystemExit("ffmpeg not found on PATH; it gets bundled into the app (winget install Gyan.FFmpeg)")
ffmpeg_bin = [(str(Path(ffmpeg_path).resolve()), "ffmpeg")]
# yt-dlp ships as plain files (not frozen) so the in-app updater can replace it with a newer copy
ytdlp_dir = sp / "yt_dlp"
ytdlp_datas = [(str(ytdlp_dir), "ytdlp_bundled/yt_dlp")]

a = Analysis(
    ["run_app.py"],
    pathex=["."],
    binaries=ffmpeg_bin + collect_dynamic_libs("ctranslate2") + collect_dynamic_libs("winrt")
             + collect_dynamic_libs("onnxruntime"),
    datas=[("streamtranslator/ui", "streamtranslator/ui"), ("assets/icon.ico", "assets")]
          + collect_data_files("faster_whisper") + ytdlp_datas,
    hiddenimports=collect_submodules("winrt") + collect_submodules("streamtranslator") + ["sentencepiece"],
    hookspath=[],
    runtime_hooks=[],
    excludes=["tkinter", "matplotlib", "IPython", "pytest", "torch",
              "PySide6.Qt3DCore", "PySide6.Qt3DRender", "PySide6.QtCharts", "PySide6.QtDataVisualization",
              "PySide6.QtBluetooth", "PySide6.QtSensors", "PySide6.QtSerialPort", "PySide6.QtTest",
              "PySide6.QtDesigner", "PySide6.QtMultimedia", "PySide6.QtQuick3D", "PySide6.QtGraphs"],
    noarchive=False,
)
# never ship NVIDIA DLLs, even if dependency scanning found some (the setup screen provides them)
_nv = ("cublas", "cudnn", "cufft", "curand", "cudart", "nvrtc", "cusparse", "cusolver")
a.binaries = [b for b in a.binaries if not any(Path(b[0]).name.lower().startswith(p) for p in _nv)]
# keep yt-dlp out of the frozen archive (it's shipped as files above)
a.pure = [m for m in a.pure if not (m[0] == "yt_dlp" or m[0].startswith("yt_dlp."))]
pyz = PYZ(a.pure)

exe = EXE(
    pyz, a.scripts, [],
    exclude_binaries=True,
    name="StreamTranslator",
    console=False,
    upx=False,
    icon=["assets/icon.ico"],
)
coll = COLLECT(exe, a.binaries, a.datas, upx=False, name="StreamTranslator")
