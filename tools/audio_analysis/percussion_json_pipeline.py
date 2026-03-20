#!/usr/bin/env python3
"""Compatibility wrapper -- delegates to manifold_audio package."""
import sys
import os

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from manifold_audio.cli import main  # noqa: E402

if __name__ == "__main__":
    raise SystemExit(main())
