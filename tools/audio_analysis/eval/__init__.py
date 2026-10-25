"""Offline accuracy eval harness for the audio analysis pipeline.

Sibling package to ``manifold_audio`` — imports it one-way (eval -> manifold_audio,
never the reverse). Plain Python, runs under the same bundled runtime.

Metrics (metrics.py) are frozen at P1 — any change is a Peter escalation.
"""

from __future__ import annotations

PIPELINE_VERSION = "p1-2026-07-17"
