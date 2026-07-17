"""Offline accuracy eval harness for the audio analysis pipeline.

Sibling package to ``manifold_audio`` — imports it one-way (eval -> manifold_audio,
never the reverse). Plain Python, runs under the same bundled runtime.

See docs/AUDIO_ANALYSIS_ACCURACY_DESIGN.md for the design: D1-D14 decisions,
the harness layout (§3), detector specs (§4), and phasing (§5). Metrics
(metrics.py) are frozen at P1 per D10 — any change is a Peter escalation.
"""

from __future__ import annotations

PIPELINE_VERSION = "p1-2026-07-17"
