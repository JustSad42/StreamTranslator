# -*- mode: python ; coding: utf-8 -*-
# PyInstaller recipe for StreamTranslator.exe (one folder). Run through build.ps1.
import os
import site
from pathlib import Path

from PyInstaller.utils.hooks import collect_data_files, collect_dynamic_libs, collect_submodules

sp = Path(site.getsitepackages()[-1]) if (Path(site.getsitepackages()[-1]) / "nvidia").exists() else \
    next(Path(p) for p in site.getsitepackages() if (Path(p) / "nvidia").exists())

# NVIDIA runtime libraries (cuBLAS, cuDNN, CUDA runtime, cuFFT, cuRAND) for Whisper + voice isolation
nvidia_bins = []
for dll in (sp / "nvidia").glob("*/bin/*.dll"):
    if "nvrtc" in dll.name:
        continue
    nvidia_bins.append((str(dll), f"nvidia/{dll.parent.parent.name}/bin"))

# yt-dlp ships as plain files (not frozen) so the in-app updater can replace it with a newer copy
ytdlp_dir = sp / "yt_dlp"
ytdlp_datas = [(str(ytdlp_dir), "ytdlp_bundled/yt_dlp")]

a = Analysis(
    ["run_app.py"],
    pathex=["."],
    binaries=nvidia_bins + collect_dynamic_libs("ctranslate2") + collect_dynamic_libs("winrt"),
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
