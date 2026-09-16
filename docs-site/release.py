"""Build the native Honeycomb package using the shared release entry point."""
from pathlib import Path
import runpy
runpy.run_path(str(Path(__file__).resolve().parents[1] / "scripts/package-release.py"), run_name="__main__")
