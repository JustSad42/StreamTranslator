"""StreamTranslator - double-click to open the window.

Re-launches itself with the project's own .venv so the right packages
(Whisper, CUDA libraries, PySide6) are used.
"""
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
VENV = HERE / ".venv" / "Scripts" / "pythonw.exe"

if VENV.exists() and Path(sys.executable).resolve() != VENV.resolve():
    subprocess.Popen([str(VENV), str(Path(__file__).resolve()), *sys.argv[1:]], cwd=HERE)
    sys.exit(0)

sys.path.insert(0, str(HERE))
from streamtranslator.app import main  # noqa: E402

sys.exit(main())
