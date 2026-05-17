#!/usr/bin/env python3
"""
audit_rename.py — apply naming/UX audit edits to the codebase.

Each rename is a `(desc, file, find, replace)` tuple. `find` must match
EXACTLY ONCE in `file`; otherwise the script reports an error without
writing. Already-applied renames (`find` absent, `replace` present)
report "[skip-applied]" — running the script twice is safe.

This is the reusable tool the §9 audit calls for in
`docs/PRIMITIVE_LIBRARY_DESIGN.md`. Future audits extend `RENAMES`
below; the apply / dry-run / report machinery doesn't change.

After all renames apply, the script:
  1. Runs `cargo test -p manifold-renderer --test bundled_presets_drift
     -- --ignored` to regenerate the bundled effect-preset JSON.
  2. Runs `cargo test --workspace --lib` to gate the diff.

Usage:
  python3 scripts/audit_rename.py             # apply
  python3 scripts/audit_rename.py --dry-run   # show what would change
  python3 scripts/audit_rename.py --no-regen  # skip preset regen + tests
"""

from __future__ import annotations

import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DRY_RUN = "--dry-run" in sys.argv
NO_REGEN = "--no-regen" in sys.argv


@dataclass
class Rename:
    """One source-code edit: change `old` to `new` in `file`, exactly once."""

    desc: str
    file: Path
    old: str
    new: str


# Path helpers — keep the rename table readable.

def effect(name: str) -> Path:
    return REPO_ROOT / "crates" / "manifold-renderer" / "src" / "effects" / f"{name}.rs"


def primitive(name: str) -> Path:
    return REPO_ROOT / "crates" / "manifold-renderer" / "src" / "node_graph" / "primitives" / f"{name}.rs"


def gen_metadata() -> Path:
    return REPO_ROOT / "crates" / "manifold-core" / "src" / "generator_metadata_submissions.rs"


def generator(name: str) -> Path:
    return REPO_ROOT / "crates" / "manifold-renderer" / "src" / "generators" / f"{name}.rs"


# ============================================================================
# Rename table
# ============================================================================
#
# Grouped by audit layer + change kind. Order matters within a logical group
# (e.g., updating a ParamSpec's id, then updating the matching ParamBinding).

RENAMES: list[Rename] = [
    # ========================================================================
    # Phase 1 — Label-only renames (safe; no id changes, no migration).
    #
    # Each effect/generator/primitive that had truncated labels gets the
    # full English form. Param ids stay the same, so saved projects keep
    # loading without an alias entry. Labels appear in:
    #   - ParamSpec::*("id", "Label", ...) in EffectMetadata / GeneratorMetadata
    #   - ParamBinding { id: ..., label: "Label", ... } in ChainSpec.bindings
    #   - ParamDef { name: "id", label: "Label", ... } in primitive declarations
    # Each label rename touches one or both of these per locus.
    # ========================================================================

    # ── Kaleidoscope: Segs → Segments ──────────────────────────────────
    Rename(
        desc="Kaleidoscope label: Segs → Segments (ParamSpec)",
        file=effect("kaleidoscope"),
        old='ParamSpec::whole("segs", "Segs", 2.0, 16.0, 6.0, "Segments"),',
        new='ParamSpec::whole("segs", "Segments", 2.0, 16.0, 6.0, "Segments"),',
    ),
    Rename(
        desc="Kaleidoscope label: Segs → Segments (ParamBinding)",
        file=effect("kaleidoscope"),
        old='''ParamBinding {
            id: Cow::Borrowed("segs"),
            label: "Segs",''',
        new='''ParamBinding {
            id: Cow::Borrowed("segs"),
            label: "Segments",''',
    ),

    # ── Edge Stretch: Dir → Direction ──────────────────────────────────
    Rename(
        desc="Edge Stretch label: Dir → Direction (ParamSpec)",
        file=effect("edge_stretch"),
        old='ParamSpec::whole_labels("dir", "Dir", 0.0, 2.0, 0.0, &["Horiz", "Vert", "Both"], "Direction"),',
        new='ParamSpec::whole_labels("dir", "Direction", 0.0, 2.0, 0.0, &["Horiz", "Vert", "Both"], "Direction"),',
    ),
    Rename(
        desc="Edge Stretch label: Dir → Direction (ParamBinding)",
        file=effect("edge_stretch"),
        old='''ParamBinding {
            id: Cow::Borrowed("dir"),
            label: "Dir",''',
        new='''ParamBinding {
            id: Cow::Borrowed("dir"),
            label: "Direction",''',
    ),

    # ── Dither: Algo → Pattern ─────────────────────────────────────────
    Rename(
        desc="Dither label: Algo → Pattern (ParamSpec)",
        file=effect("dither"),
        old='ParamSpec::whole_labels("algo", "Algo", 0.0, 5.0, 0.0, &["Bayer", "Halftone", "Lines", "X-Hatch", "Noise", "Diamond"], "Algorithm"),',
        new='ParamSpec::whole_labels("algo", "Pattern", 0.0, 5.0, 0.0, &["Bayer", "Halftone", "Lines", "X-Hatch", "Noise", "Diamond"], "Algorithm"),',
    ),
    Rename(
        desc="Dither label: Algo → Pattern (ParamBinding)",
        file=effect("dither"),
        old='''ParamBinding {
            id: Cow::Borrowed("algo"),
            label: "Algo",''',
        new='''ParamBinding {
            id: Cow::Borrowed("algo"),
            label: "Pattern",''',
    ),

    # ── HDR Boost: Thresh → Threshold ──────────────────────────────────
    Rename(
        desc="HDR Boost label: Thresh → Threshold (ParamSpec)",
        file=effect("hdr_boost"),
        old='ParamSpec::continuous("thresh", "Thresh", 0.0, 1.0, 0.15, "F2", "Threshold"),',
        new='ParamSpec::continuous("thresh", "Threshold", 0.0, 1.0, 0.15, "F2", "Threshold"),',
    ),
    Rename(
        desc="HDR Boost label: Thresh → Threshold (ParamBinding)",
        file=effect("hdr_boost"),
        old='''ParamBinding {
            id: Cow::Borrowed("thresh"),
            label: "Thresh",''',
        new='''ParamBinding {
            id: Cow::Borrowed("thresh"),
            label: "Threshold",''',
    ),

    # ── Edge Detect: Thresh → Threshold ────────────────────────────────
    Rename(
        desc="Edge Detect label: Thresh → Threshold (ParamSpec)",
        file=effect("edge_detect"),
        old='ParamSpec::continuous("thresh", "Thresh", 0.0, 1.0, 0.1, "F2", "Threshold"),',
        new='ParamSpec::continuous("thresh", "Threshold", 0.0, 1.0, 0.1, "F2", "Threshold"),',
    ),
    Rename(
        desc="Edge Detect label: Thresh → Threshold (ParamBinding)",
        file=effect("edge_detect"),
        old='''ParamBinding {
            id: Cow::Borrowed("thresh"),
            label: "Thresh",''',
        new='''ParamBinding {
            id: Cow::Borrowed("thresh"),
            label: "Threshold",''',
    ),

    # ── Halation: Thresh → Threshold, Sat → Saturation ─────────────────
    Rename(
        desc="Halation label: Thresh → Threshold (ParamSpec)",
        file=effect("halation"),
        old='ParamSpec::continuous("thresh", "Thresh", 0.0, 1.0, 0.5, "F2", "Threshold"),',
        new='ParamSpec::continuous("thresh", "Threshold", 0.0, 1.0, 0.5, "F2", "Threshold"),',
    ),
    Rename(
        desc="Halation label: Sat → Saturation (ParamSpec)",
        file=effect("halation"),
        old='ParamSpec::continuous("sat", "Sat", 0.0, 1.0, 0.6, "F2", "Saturation"),',
        new='ParamSpec::continuous("sat", "Saturation", 0.0, 1.0, 0.6, "F2", "Saturation"),',
    ),
    Rename(
        desc="Halation label: Thresh → Threshold (ParamBinding)",
        file=effect("halation"),
        old='''ParamBinding {
            id: Cow::Borrowed("thresh"),
            label: "Thresh",''',
        new='''ParamBinding {
            id: Cow::Borrowed("thresh"),
            label: "Threshold",''',
    ),
    Rename(
        desc="Halation label: Sat → Saturation (ParamBinding)",
        file=effect("halation"),
        old='''ParamBinding {
            id: Cow::Borrowed("sat"),
            label: "Sat",''',
        new='''ParamBinding {
            id: Cow::Borrowed("sat"),
            label: "Saturation",''',
    ),

    # ── Color Grade: Sat → Saturation, TintHue/TintSat → Tint Hue/Tint Saturation, Focus → Tint Focus ──
    Rename(
        desc="Color Grade label: Sat → Saturation (ParamSpec)",
        file=effect("color_grade"),
        old='ParamSpec::continuous("sat", "Sat", 0.0, 2.0, 1.0, "F2", "Saturation"),',
        new='ParamSpec::continuous("sat", "Saturation", 0.0, 2.0, 1.0, "F2", "Saturation"),',
    ),
    Rename(
        desc="Color Grade label: TintHue → Tint Hue (ParamSpec)",
        file=effect("color_grade"),
        old='ParamSpec::continuous("tint_hue", "TintHue", 0.0, 360.0, 0.0, "F2", "TintHue"),',
        new='ParamSpec::continuous("tint_hue", "Tint Hue", 0.0, 360.0, 0.0, "F2", "TintHue"),',
    ),
    Rename(
        desc="Color Grade label: TintSat → Tint Saturation (ParamSpec)",
        file=effect("color_grade"),
        old='ParamSpec::continuous("tint_sat", "TintSat", 0.0, 2.0, 1.0, "F2", "TintSaturation"),',
        new='ParamSpec::continuous("tint_sat", "Tint Saturation", 0.0, 2.0, 1.0, "F2", "TintSaturation"),',
    ),
    Rename(
        desc="Color Grade label: Focus → Tint Focus (ParamSpec)",
        file=effect("color_grade"),
        old='ParamSpec::continuous("focus", "Focus", 0.0, 1.0, 0.75, "F2", "ColorizeFocus"),',
        new='ParamSpec::continuous("focus", "Tint Focus", 0.0, 1.0, 0.75, "F2", "ColorizeFocus"),',
    ),
    Rename(
        desc="Color Grade label: Sat → Saturation (ParamBinding)",
        file=effect("color_grade"),
        old='''ParamBinding {
            id: Cow::Borrowed("sat"),
            label: "Sat",''',
        new='''ParamBinding {
            id: Cow::Borrowed("sat"),
            label: "Saturation",''',
    ),
    Rename(
        desc="Color Grade label: TintHue → Tint Hue (ParamBinding)",
        file=effect("color_grade"),
        old='''ParamBinding {
            id: Cow::Borrowed("tint_hue"),
            label: "TintHue",''',
        new='''ParamBinding {
            id: Cow::Borrowed("tint_hue"),
            label: "Tint Hue",''',
    ),
    Rename(
        desc="Color Grade label: TintSat → Tint Saturation (ParamBinding)",
        file=effect("color_grade"),
        old='''ParamBinding {
            id: Cow::Borrowed("tint_sat"),
            label: "TintSat",''',
        new='''ParamBinding {
            id: Cow::Borrowed("tint_sat"),
            label: "Tint Saturation",''',
    ),
    Rename(
        desc="Color Grade label: Focus → Tint Focus (ParamBinding)",
        file=effect("color_grade"),
        old='''ParamBinding {
            id: Cow::Borrowed("focus"),
            label: "Focus",''',
        new='''ParamBinding {
            id: Cow::Borrowed("focus"),
            label: "Tint Focus",''',
    ),

    # ── Transform: Rot → Rotation ──────────────────────────────────────
    Rename(
        desc="Transform label: Rot → Rotation (ParamSpec)",
        file=effect("transform"),
        old='ParamSpec::continuous("rot", "Rot", -180.0, 180.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("rot", "Rotation", -180.0, 180.0, 0.0, "F2", ""),',
    ),
    Rename(
        desc="Transform label: Rot → Rotation (ParamBinding)",
        file=effect("transform"),
        old='''ParamBinding {
            id: Cow::Borrowed("rot"),
            label: "Rot",''',
        new='''ParamBinding {
            id: Cow::Borrowed("rot"),
            label: "Rotation",''',
    ),

    # ── Auto Gain: Char → Character, HDR Ret → HDR Retention ───────────
    Rename(
        desc="Auto Gain label: Char → Character (ParamSpec)",
        file=effect("auto_gain"),
        old='ParamSpec::whole_labels("char", "Char", 0.0, 4.0, 0.0, &["Clean", "Warm", "Film", "Vivid", "Grit"], "Character"),',
        new='ParamSpec::whole_labels("char", "Character", 0.0, 4.0, 0.0, &["Clean", "Warm", "Film", "Vivid", "Grit"], "Character"),',
    ),
    Rename(
        desc="Auto Gain label: HDR Ret → HDR Retention (ParamSpec)",
        file=effect("auto_gain"),
        old='ParamSpec::continuous("hdr_ret", "HDR Ret", 0.0, 1.0, 0.5, "F2", "HdrRetention"),',
        new='ParamSpec::continuous("hdr_ret", "HDR Retention", 0.0, 1.0, 0.5, "F2", "HdrRetention"),',
    ),
    Rename(
        desc="Auto Gain label: Char → Character (ParamBinding)",
        file=effect("auto_gain"),
        old='''ParamBinding {
            id: Cow::Borrowed("char"),
            label: "Char",''',
        new='''ParamBinding {
            id: Cow::Borrowed("char"),
            label: "Character",''',
    ),
    Rename(
        desc="Auto Gain label: HDR Ret → HDR Retention (ParamBinding)",
        file=effect("auto_gain"),
        old='''ParamBinding {
            id: Cow::Borrowed("hdr_ret"),
            label: "HDR Ret",''',
        new='''ParamBinding {
            id: Cow::Borrowed("hdr_ret"),
            label: "HDR Retention",''',
    ),

    # ── Blob Track: Sens → Sensitivity, Smooth → Smoothing ─────────────
    Rename(
        desc="Blob Track label: Sens → Sensitivity (ParamSpec)",
        file=effect("blob_tracking"),
        old='ParamSpec::continuous("sens", "Sens", 0.2, 1.0, 0.85, "F2", "Sensitivity"),',
        new='ParamSpec::continuous("sens", "Sensitivity", 0.2, 1.0, 0.85, "F2", "Sensitivity"),',
    ),
    Rename(
        desc="Blob Track label: Smooth → Smoothing (ParamSpec)",
        file=effect("blob_tracking"),
        old='ParamSpec::continuous("smooth", "Smooth", 0.0, 1.0, 0.7, "F2", "Smoothing"),',
        new='ParamSpec::continuous("smooth", "Smoothing", 0.0, 1.0, 0.7, "F2", "Smoothing"),',
    ),
    Rename(
        desc="Blob Track label: Thresh → Threshold (ParamSpec)",
        file=effect("blob_tracking"),
        old='ParamSpec::continuous("thresh", "Thresh", 0.05, 0.9, 0.65, "F2", "Threshold"),',
        new='ParamSpec::continuous("thresh", "Threshold", 0.05, 0.9, 0.65, "F2", "Threshold"),',
    ),
    Rename(
        desc="Blob Track label: Sens → Sensitivity (ParamBinding)",
        file=effect("blob_tracking"),
        old='''ParamBinding {
            id: Cow::Borrowed("sens"),
            label: "Sens",''',
        new='''ParamBinding {
            id: Cow::Borrowed("sens"),
            label: "Sensitivity",''',
    ),
    Rename(
        desc="Blob Track label: Smooth → Smoothing (ParamBinding)",
        file=effect("blob_tracking"),
        old='''ParamBinding {
            id: Cow::Borrowed("smooth"),
            label: "Smooth",''',
        new='''ParamBinding {
            id: Cow::Borrowed("smooth"),
            label: "Smoothing",''',
    ),
    Rename(
        desc="Blob Track label: Thresh → Threshold (ParamBinding)",
        file=effect("blob_tracking"),
        old='''ParamBinding {
            id: Cow::Borrowed("thresh"),
            label: "Thresh",''',
        new='''ParamBinding {
            id: Cow::Borrowed("thresh"),
            label: "Threshold",''',
    ),

    # ── Wireframe Depth: ZScale → Z Scale, WireRes → Wire Resolution, MeshRate → Mesh Rate, EdgeFollow → Edge Follow ──
    Rename(
        desc="Wireframe Depth label: ZScale → Z Scale (ParamSpec)",
        file=effect("wireframe_depth"),
        old='ParamSpec::continuous("z_scale", "ZScale", 0.0, 2.5, 1.35, "F2", "ZScale"),',
        new='ParamSpec::continuous("z_scale", "Z Scale", 0.0, 2.5, 1.35, "F2", "ZScale"),',
    ),
    Rename(
        desc="Wireframe Depth label: WireRes → Wire Resolution (ParamSpec)",
        file=effect("wireframe_depth"),
        old='ParamSpec::continuous("wire_res", "WireRes", 0.5, 1.0, 1.0, "F2", "WireRes"),',
        new='ParamSpec::continuous("wire_res", "Wire Resolution", 0.5, 1.0, 1.0, "F2", "WireRes"),',
    ),
    Rename(
        desc="Wireframe Depth label: MeshRate → Mesh Rate (ParamSpec)",
        file=effect("wireframe_depth"),
        old='ParamSpec::whole_labels("mesh_rate", "MeshRate", 1.0, 4.0, 1.0, &["Every", "Half", "Third", "Quarter"], "MeshRate"),',
        new='ParamSpec::whole_labels("mesh_rate", "Mesh Rate", 1.0, 4.0, 1.0, &["Every", "Half", "Third", "Quarter"], "MeshRate"),',
    ),
    Rename(
        desc="Wireframe Depth label: EdgeFollow → Edge Follow (ParamSpec)",
        file=effect("wireframe_depth"),
        old='ParamSpec::continuous("edge_follow", "EdgeFollow", 0.0, 1.0, 0.5, "F2", "EdgeFollow"),',
        new='ParamSpec::continuous("edge_follow", "Edge Follow", 0.0, 1.0, 0.5, "F2", "EdgeFollow"),',
    ),
    Rename(
        desc="Wireframe Depth label: ZScale → Z Scale (ParamBinding)",
        file=effect("wireframe_depth"),
        old='''ParamBinding {
            id: Cow::Borrowed("z_scale"),
            label: "ZScale",''',
        new='''ParamBinding {
            id: Cow::Borrowed("z_scale"),
            label: "Z Scale",''',
    ),
    Rename(
        desc="Wireframe Depth label: WireRes → Wire Resolution (ParamBinding)",
        file=effect("wireframe_depth"),
        old='''ParamBinding {
            id: Cow::Borrowed("wire_res"),
            label: "WireRes",''',
        new='''ParamBinding {
            id: Cow::Borrowed("wire_res"),
            label: "Wire Resolution",''',
    ),
    Rename(
        desc="Wireframe Depth label: MeshRate → Mesh Rate (ParamBinding)",
        file=effect("wireframe_depth"),
        old='''ParamBinding {
            id: Cow::Borrowed("mesh_rate"),
            label: "MeshRate",''',
        new='''ParamBinding {
            id: Cow::Borrowed("mesh_rate"),
            label: "Mesh Rate",''',
    ),
    Rename(
        desc="Wireframe Depth label: EdgeFollow → Edge Follow (ParamBinding)",
        file=effect("wireframe_depth"),
        old='''ParamBinding {
            id: Cow::Borrowed("edge_follow"),
            label: "EdgeFollow",''',
        new='''ParamBinding {
            id: Cow::Borrowed("edge_follow"),
            label: "Edge Follow",''',
    ),

    # ── Glitch: Block → Block Size ─────────────────────────────────────
    Rename(
        desc="Glitch label: Block → Block Size (ParamSpec)",
        file=effect("glitch"),
        old='ParamSpec::continuous("block", "Block", 4.0, 64.0, 16.0, "F2", "BlockSize"),',
        new='ParamSpec::continuous("block", "Block Size", 4.0, 64.0, 16.0, "F2", "BlockSize"),',
    ),
    Rename(
        desc="Glitch label: Block → Block Size (ParamBinding)",
        file=effect("glitch"),
        old='''ParamBinding {
            id: Cow::Borrowed("block"),
            label: "Block",''',
        new='''ParamBinding {
            id: Cow::Borrowed("block"),
            label: "Block Size",''',
    ),

    # ========================================================================
    # Layer 2 — Primitive-side label-only fixes (§9.2.3)
    # ========================================================================

    # ── node.threshold: rename label match the id meaning (label "Threshold" stays, id `level` rename deferred to id-batch) ──

    # ── node.wet_dry: "Wet / Dry" → "Wet/Dry" ──────────────────────────
    Rename(
        desc="node.wet_dry label: 'Wet / Dry' → 'Wet/Dry'",
        file=primitive("wet_dry_mix"),
        old='label: "Wet / Dry",',
        new='label: "Wet/Dry",',
    ),

    # ── node.chromatic_aberration angle: drop "(deg)" — unit lives elsewhere ──
    Rename(
        desc="node.chromatic_aberration label: 'Angle (deg)' → 'Angle'",
        file=primitive("chromatic_offset"),
        old='label: "Angle (deg)",',
        new='label: "Angle",',
    ),

    # ── node.gaussian_blur kernel_size: "Kernel" → "Kernel Size" ───────
    Rename(
        desc="node.gaussian_blur label: 'Kernel' → 'Kernel Size'",
        file=primitive("separable_gaussian"),
        old='label: "Kernel",',
        new='label: "Kernel Size",',
    ),

    # ── node.wireframe_depth.smooth: label "Smooth" → "Smoothing" ──────
    Rename(
        desc="node.wireframe_depth label: Smooth → Smoothing",
        file=primitive("wireframe_depth"),
        old='''name: "smooth",
        label: "Smooth",''',
        new='''name: "smooth",
        label: "Smoothing",''',
    ),

    # ========================================================================
    # Phase 2 — Default-value changes (UX calls 1, 4, 5).
    #
    # No migration needed: defaults only affect new instances. Existing
    # projects keep their saved values. Each default change touches the
    # ParamSpec literal AND the ParamBinding's default_value field — must
    # stay in sync (validated by tests).
    # ========================================================================

    # ── §9.1.5 Amount rule: 12 effects flip amount=0 → 1.0 ──────────────

    # Kaleidoscope amount: 0.0 → 1.0
    Rename(
        desc="Kaleidoscope amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("kaleidoscope"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="Kaleidoscope amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("kaleidoscope"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # Edge Detect amount: 0.0 → 1.0
    Rename(
        desc="Edge Detect amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("edge_detect"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="Edge Detect amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("edge_detect"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # Dither amount: 0.0 → 1.0
    Rename(
        desc="Dither amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("dither"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="Dither amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("dither"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # Halation amount: 0.0 → 1.0
    Rename(
        desc="Halation amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("halation"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="Halation amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("halation"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # Glitch amount: 0.0 → 1.0
    Rename(
        desc="Glitch amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("glitch"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="Glitch amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("glitch"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # Strobe amount: 0.0 → 1.0
    Rename(
        desc="Strobe amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("strobe"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="Strobe amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("strobe"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # Voronoi Prism amount: 0.0 → 1.0
    Rename(
        desc="Voronoi Prism amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("voronoi_prism"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="Voronoi Prism amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("voronoi_prism"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # Chromatic Aberration amount: 0.0 → 1.0
    Rename(
        desc="Chromatic Aberration amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("chromatic_aberration"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="Chromatic Aberration amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("chromatic_aberration"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # Color Grade amount: 0.0 → 1.0
    Rename(
        desc="Color Grade amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("color_grade"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="Color Grade amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("color_grade"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # Infrared amount: 0.0 → 1.0
    Rename(
        desc="Infrared amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("infrared"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="Infrared amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("infrared"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # HDR Boost amount: 0.0 → 1.0
    Rename(
        desc="HDR Boost amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("hdr_boost"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="HDR Boost amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("hdr_boost"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # Depth of Field amount: 0.0 → 1.0
    Rename(
        desc="DoF amount: 0.0 → 1.0 (ParamSpec)",
        file=effect("depth_of_field"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 1.0, "F2", ""),',
    ),
    Rename(
        desc="DoF amount: 0.0 → 1.0 (ParamBinding default)",
        file=effect("depth_of_field"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 1.0,''',
    ),

    # Blob Track amount: 0.0 → 0.5 (subtle continuum per §9.1.5)
    Rename(
        desc="Blob Track amount: 0.0 → 0.5 (ParamSpec)",
        file=effect("blob_tracking"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", ""),',
    ),
    Rename(
        desc="Blob Track amount: 0.0 → 0.5 (ParamBinding default)",
        file=effect("blob_tracking"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.0,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.5,''',
    ),

    # ── §9.1.7 Magic-number defaults — round to clean values ───────────

    # Edge Stretch width: 0.433 → 0.5
    Rename(
        desc="Edge Stretch width default: 0.433 → 0.5 (ParamSpec)",
        file=effect("edge_stretch"),
        old='ParamSpec::continuous("width", "Width", 0.1, 0.9, 0.433, "F2", "SourceWidth"),',
        new='ParamSpec::continuous("width", "Width", 0.1, 0.9, 0.5, "F2", "SourceWidth"),',
    ),
    Rename(
        desc="Edge Stretch width default: 0.433 → 0.5 (ParamBinding)",
        file=effect("edge_stretch"),
        old='''ParamBinding {
            id: Cow::Borrowed("width"),
            label: "Width",
            default_value: 0.433,''',
        new='''ParamBinding {
            id: Cow::Borrowed("width"),
            label: "Width",
            default_value: 0.5,''',
    ),
    Rename(
        desc="ClampStretch primitive: source_width default 0.433 → 0.5",
        file=primitive("clamp_stretch"),
        old='default: ParamValue::Float(0.433),',
        new='default: ParamValue::Float(0.5),',
    ),

    # Voronoi Prism source_width: 0.5625 → 0.5
    Rename(
        desc="Voronoi Prism source_width default: 0.5625 → 0.5 (ParamSpec)",
        file=effect("voronoi_prism"),
        old='ParamSpec::continuous("source_width", "Cell Size", 0.1, 1.0, 0.5625, "F2", "SourceWidth"),',
        new='ParamSpec::continuous("source_width", "Cell Size", 0.1, 1.0, 0.5, "F2", "SourceWidth"),',
    ),
    Rename(
        desc="Voronoi Prism source_width default: 0.5625 → 0.5 (ParamBinding)",
        file=effect("voronoi_prism"),
        old='''ParamBinding {
            id: Cow::Borrowed("source_width"),
            label: "Cell Size",
            default_value: 0.5625,''',
        new='''ParamBinding {
            id: Cow::Borrowed("source_width"),
            label: "Cell Size",
            default_value: 0.5,''',
    ),
    Rename(
        desc="VoronoiPrism primitive: source_width default 0.5625 → 0.5",
        file=primitive("voronoi_prism"),
        old='default: ParamValue::Float(0.5625),',
        new='default: ParamValue::Float(0.5),',
    ),

    # Bloom amount: 0.187 → 0.5
    Rename(
        desc="Bloom amount default: 0.187 → 0.5 (ParamSpec)",
        file=effect("bloom"),
        old='ParamSpec::continuous("amount", "Amount", 0.0, 5.0, 0.187, "F2", ""),',
        new='ParamSpec::continuous("amount", "Amount", 0.0, 5.0, 0.5, "F2", ""),',
    ),
    Rename(
        desc="Bloom amount default: 0.187 → 0.5 (ParamBinding)",
        file=effect("bloom"),
        old='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.187,''',
        new='''ParamBinding {
            id: Cow::Borrowed("amount"),
            label: "Amount",
            default_value: 0.5,''',
    ),
    Rename(
        desc="Bloom primitive: amount default 0.187 → 0.5",
        file=primitive("bloom"),
        old='default: ParamValue::Float(0.187),',
        new='default: ParamValue::Float(0.5),',
    ),
    Rename(
        desc="Bloom apply(): fallback 0.187 → 0.5",
        file=effect("bloom"),
        old='let amount = fx.param_values.first().map(|p| p.value).unwrap_or(0.187);',
        new='let amount = fx.param_values.first().map(|p| p.value).unwrap_or(0.5);',
    ),

    # Wireframe Depth width: 1.335 → 1.5
    Rename(
        desc="Wireframe Depth width default: 1.335 → 1.5 (ParamSpec)",
        file=effect("wireframe_depth"),
        old='ParamSpec::continuous("width", "Width", 0.4, 3.0, 1.335, "F2", "Width"),',
        new='ParamSpec::continuous("width", "Width", 0.4, 3.0, 1.5, "F2", "Width"),',
    ),
    Rename(
        desc="Wireframe Depth width default: 1.335 → 1.5 (ParamBinding)",
        file=effect("wireframe_depth"),
        old='''ParamBinding {
            id: Cow::Borrowed("width"),
            label: "Width",
            default_value: 1.335,''',
        new='''ParamBinding {
            id: Cow::Borrowed("width"),
            label: "Width",
            default_value: 1.5,''',
    ),
    Rename(
        desc="WireframeDepth primitive: width default 1.335 → 1.5",
        file=primitive("wireframe_depth"),
        old='''name: "width",
        label: "Width",
        ty: ParamType::Float,
        default: ParamValue::Float(1.335),''',
        new='''name: "width",
        label: "Width",
        ty: ParamType::Float,
        default: ParamValue::Float(1.5),''',
    ),

    # Wireframe Depth subject: 0.52 → 0.5
    Rename(
        desc="Wireframe Depth subject default: 0.52 → 0.5 (ParamSpec)",
        file=effect("wireframe_depth"),
        old='ParamSpec::continuous("subject", "Subject", 0.0, 1.0, 0.52, "F2", "SubjectIsolation"),',
        new='ParamSpec::continuous("subject", "Subject", 0.0, 1.0, 0.5, "F2", "SubjectIsolation"),',
    ),
    Rename(
        desc="Wireframe Depth subject default: 0.52 → 0.5 (ParamBinding)",
        file=effect("wireframe_depth"),
        old='''ParamBinding {
            id: Cow::Borrowed("subject"),
            label: "Subject",
            default_value: 0.52,''',
        new='''ParamBinding {
            id: Cow::Borrowed("subject"),
            label: "Subject",
            default_value: 0.5,''',
    ),
    Rename(
        desc="WireframeDepth primitive: subject default 0.52 → 0.5",
        file=primitive("wireframe_depth"),
        old='''name: "subject",
        label: "Subject",
        ty: ParamType::Float,
        default: ParamValue::Float(0.52),''',
        new='''name: "subject",
        label: "Subject",
        ty: ParamType::Float,
        default: ParamValue::Float(0.5),''',
    ),

    # Wireframe Depth smooth range: [0, 0.98] → [0, 1.0]
    Rename(
        desc="Wireframe Depth smooth range: [0, 0.98] → [0, 1.0] (ParamSpec)",
        file=effect("wireframe_depth"),
        old='ParamSpec::continuous("smooth", "Smooth", 0.0, 0.98, 0.90, "F2", "Smooth"),',
        new='ParamSpec::continuous("smooth", "Smooth", 0.0, 1.0, 0.90, "F2", "Smooth"),',
    ),
    Rename(
        desc="WireframeDepth primitive: smooth range [0, 0.98] → [0, 1.0]",
        file=primitive("wireframe_depth"),
        old='''name: "smooth",
        label: "Smoothing",
        ty: ParamType::Float,
        default: ParamValue::Float(0.90),
        range: Some((0.0, 0.98)),''',
        new='''name: "smooth",
        label: "Smoothing",
        ty: ParamType::Float,
        default: ParamValue::Float(0.90),
        range: Some((0.0, 1.0)),''',
    ),

    # ── §9.3.3 Generator snap default rule: 4 generators flip 1.0 → 0.0 ──

    Rename(
        desc="Plasma snap default: 1.0 → 0.0",
        file=gen_metadata(),
        old='ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 1.0, "snap"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Basic Shapes Snap ',
        new='ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 0.0, "snap"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Basic Shapes Snap ',
    ),
    Rename(
        desc="Lissajous snap default: 1.0 → 0.0",
        file=gen_metadata(),
        old='ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 1.0, "snap"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Wireframe Zoo ',
        new='ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 0.0, "snap"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Wireframe Zoo ',
    ),
    Rename(
        desc="Oscilloscope XY snap default: 1.0 → 0.0",
        file=gen_metadata(),
        old='ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 1.0, "snap"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Parametric Surface ',
        new='ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 0.0, "snap"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Parametric Surface ',
    ),
    Rename(
        desc="Parametric Surface snap default: 1.0 → 0.0",
        file=gen_metadata(),
        old='ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 1.0, "snap"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Mycelium ',
        new='ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 0.0, "snap"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Mycelium ',
    ),

    # ── §9.3.5 Generator magic-number defaults — round to clean values ──

    Rename(
        desc="Plasma contrast default: 0.63 → 0.5",
        file=gen_metadata(),
        old='ParamSpec::continuous("contrast", "Contrast", 0.0, 1.0, 0.63, "F2", "contrast"),',
        new='ParamSpec::continuous("contrast", "Contrast", 0.0, 1.0, 0.5, "F2", "contrast"),',
    ),

    # Mycelium color: 0.08 → 0.0 (subtle by default, user opts in to hue rotation)
    Rename(
        desc="Mycelium color default: 0.08 → 0.0",
        file=gen_metadata(),
        old='ParamSpec::continuous("color", "Color", 0.0, 1.0, 0.08, "F2", "color"),',
        new='ParamSpec::continuous("color", "Color", 0.0, 1.0, 0.0, "F2", "color"),',
    ),

    # Strange Attractor chaos: 0.0 → 0.3 (visible motion on add)
    Rename(
        desc="Strange Attractor chaos default: 0.0 → 0.3",
        file=generator("strange_attractor"),
        old='ParamSpec::continuous("chaos", "Chaos", 0.0, 1.0, 0.0, "F2", "chaos"),',
        new='ParamSpec::continuous("chaos", "Chaos", 0.0, 1.0, 0.3, "F2", "chaos"),',
    ),

    # Lissajous magic-number cleanup (§9.3.5).
    # Narrow scope: only the 3 lines that actually change defaults.
    # (Original entry included verts/anim lines for unique anchoring,
    # but Phase 4's Verts→Vertices rename invalidated that context.
    # These freq_x/freq_y/phase magic numbers are unique to Lissajous,
    # so no further scoping needed.)
    Rename(
        desc="Lissajous freq_x default: 0.13 → 0.1",
        file=gen_metadata(),
        old='ParamSpec::continuous("freq_x", "Freq X", 0.0, 2.0, 0.13, "F2", "freqX"),',
        new='ParamSpec::continuous("freq_x", "Freq X", 0.0, 2.0, 0.1, "F2", "freqX"),',
    ),
    Rename(
        desc="Lissajous freq_y default: 0.09 → 0.1",
        file=gen_metadata(),
        old='ParamSpec::continuous("freq_y", "Freq Y", 0.0, 2.0, 0.09, "F2", "freqY"),',
        new='ParamSpec::continuous("freq_y", "Freq Y", 0.0, 2.0, 0.1, "F2", "freqY"),',
    ),
    Rename(
        desc="Lissajous phase default: 0.07 → 0.0",
        file=gen_metadata(),
        old='ParamSpec::continuous("phase", "Phase", 0.0, 2.0, 0.07, "F2", "phase"),',
        new='ParamSpec::continuous("phase", "Phase", 0.0, 2.0, 0.0, "F2", "phase"),',
    ),
    Rename(
        desc="Lissajous speed default: 2.67 → 1.0",
        file=gen_metadata(),
        old='ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 2.67, "F1", "speed"),',
        new='ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),',
    ),
    Rename(
        desc="Lissajous window default: 0.74 → 0.5",
        file=gen_metadata(),
        old='ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.74, "F2", "window"),',
        new='ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.5, "F2", "window"),',
    ),
    Rename(
        desc="Lissajous scale default: 1.55 → 1.0",
        file=gen_metadata(),
        old='ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.55, "F2", "scale"),',
        new='ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),',
    ),

    # ── §9.3.5 Black Hole.freefall: F0 → F2 (format mismatch with [0,1] range) ──
    Rename(
        desc="Black Hole freefall format: F0 → F2",
        file=gen_metadata(),
        old='ParamSpec::continuous("freefall", "Freefall", 0.0, 1.0, 0.0, "F0", "freefall"),',
        new='ParamSpec::continuous("freefall", "Freefall", 0.0, 1.0, 0.0, "F2", "freefall"),',
    ),

    # ========================================================================
    # Phase 3 — Category restructure (UX call 2 / §9.1.4).
    #
    # Old buckets: Spatial / Post-Process / Filmic / Surveillance.
    # New buckets: Spatial / Color / Stylize / Filmic / Diagnostic.
    #
    # Each effect's category field updates in its EffectMetadata block.
    # The picker reads ALL_CATEGORIES from effect_type_registry.rs, which
    # changes in a follow-up direct edit (not script-friendly because it's
    # multi-line const reshuffling).
    # ========================================================================

    # ── To Color: Invert, Infrared, Dither, Color Grade ─────────────────
    Rename(
        desc="Invert category: Spatial → Color",
        file=effect("invert_colors"),
        old='category: "Spatial",',
        new='category: "Color",',
    ),
    Rename(
        desc="Infrared category: Surveillance → Color",
        file=effect("infrared"),
        old='category: "Surveillance",',
        new='category: "Color",',
    ),
    Rename(
        desc="Dither category: Post-Process → Color",
        file=effect("dither"),
        old='category: "Post-Process",',
        new='category: "Color",',
    ),
    Rename(
        desc="Color Grade category: Post-Process → Color",
        file=effect("color_grade"),
        old='category: "Post-Process",',
        new='category: "Color",',
    ),

    # ── To Spatial: Mirror, Quad Mirror, Kaleidoscope, Edge Stretch ────
    Rename(
        desc="Mirror category: Post-Process → Spatial",
        file=effect("mirror"),
        old='category: "Post-Process",',
        new='category: "Spatial",',
    ),
    Rename(
        desc="Quad Mirror category: Post-Process → Spatial",
        file=effect("quad_mirror"),
        old='category: "Post-Process",',
        new='category: "Spatial",',
    ),
    Rename(
        desc="Kaleidoscope category: Post-Process → Spatial",
        file=effect("kaleidoscope"),
        old='category: "Post-Process",',
        new='category: "Spatial",',
    ),
    Rename(
        desc="Edge Stretch category: Post-Process → Spatial",
        file=effect("edge_stretch"),
        old='category: "Post-Process",',
        new='category: "Spatial",',
    ),

    # ── To Stylize: Strobe, Stylized Feedback, Soft Focus, Watercolor,
    #               Voronoi Prism, Auto Gain ──────────────────────────────
    Rename(
        desc="Strobe category: Post-Process → Stylize",
        file=effect("strobe"),
        old='category: "Post-Process",',
        new='category: "Stylize",',
    ),
    Rename(
        desc="Stylized Feedback category: Post-Process → Stylize",
        file=effect("stylized_feedback"),
        old='category: "Post-Process",',
        new='category: "Stylize",',
    ),
    Rename(
        desc="Soft Focus category: Post-Process → Stylize",
        file=effect("soft_focus_graph"),
        old='category: "Post-Process",',
        new='category: "Stylize",',
    ),
    Rename(
        desc="Watercolor category: Post-Process → Stylize",
        file=effect("watercolor"),
        old='category: "Post-Process",',
        new='category: "Stylize",',
    ),
    Rename(
        desc="Voronoi Prism category: Post-Process → Stylize",
        file=effect("voronoi_prism"),
        old='category: "Post-Process",',
        new='category: "Stylize",',
    ),
    Rename(
        desc="Auto Gain category: Post-Process → Stylize",
        file=effect("auto_gain"),
        old='category: "Post-Process",',
        new='category: "Stylize",',
    ),

    # ── To Diagnostic: Edge Detect, Blob Track, Wireframe Depth,
    #                  NodeGraphTest (the only `available: true` debug
    #                  effect — "see the graph run") ─────────────────────
    Rename(
        desc="Edge Detect category: Post-Process → Diagnostic",
        file=effect("edge_detect"),
        old='category: "Post-Process",',
        new='category: "Diagnostic",',
    ),
    Rename(
        desc="Blob Track category: Post-Process → Diagnostic",
        file=effect("blob_tracking"),
        old='category: "Post-Process",',
        new='category: "Diagnostic",',
    ),
    Rename(
        desc="Wireframe Depth category: Post-Process → Diagnostic",
        file=effect("wireframe_depth"),
        old='category: "Post-Process",',
        new='category: "Diagnostic",',
    ),
    Rename(
        desc="Node Graph Test category: Post-Process → Diagnostic",
        file=effect("node_graph_test"),
        old='category: "Post-Process",',
        new='category: "Diagnostic",',
    ),

    # ── To Filmic: Bloom ────────────────────────────────────────────────
    Rename(
        desc="Bloom category: Post-Process → Filmic",
        file=effect("bloom"),
        old='category: "Post-Process",',
        new='category: "Filmic",',
    ),

    # ========================================================================
    # Phase 4 — Layer 3 generator label renames (§9.3.1, §9.3.4).
    #
    # Pure cosmetic: labels appear in the generator panel. No id changes,
    # so no migration needed. All single-site (ParamSpec.label only —
    # generators don't have ChainSpec.bindings).
    # ========================================================================

    # ── Verts → Vertices (5 generators) ─────────────────────────────────
    Rename(
        desc="Tesseract: Verts → Vertices",
        file=gen_metadata(),
        old='ParamSpec::toggle("verts", "Verts", 0.0, 1.0, 1.0, "verts"),\n            ParamSpec::continuous("v_size", "VSize", 0.1, 4.0, 1.0, "F1", "vsize"),\n            ParamSpec::toggle("anim", "Anim", 0.0, 1.0, 0.0, "anim"),\n            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),\n            ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.1, "F2", "window"),\n            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Duocylinder ',
        new='ParamSpec::toggle("verts", "Vertices", 0.0, 1.0, 1.0, "verts"),\n            ParamSpec::continuous("v_size", "Vertex Size", 0.1, 4.0, 1.0, "F1", "vsize"),\n            ParamSpec::toggle("anim", "Animate", 0.0, 1.0, 0.0, "anim"),\n            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),\n            ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.1, "F2", "window"),\n            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Duocylinder ',
    ),
    Rename(
        desc="Duocylinder: Verts → Vertices",
        file=gen_metadata(),
        old='ParamSpec::toggle("verts", "Verts", 0.0, 1.0, 1.0, "verts"),\n            ParamSpec::continuous("v_size", "VSize", 0.1, 4.0, 1.0, "F1", "vsize"),\n            ParamSpec::toggle("anim", "Anim", 0.0, 1.0, 0.0, "anim"),\n            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),\n            ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.1, "F2", "window"),\n            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Lissajous ',
        new='ParamSpec::toggle("verts", "Vertices", 0.0, 1.0, 1.0, "verts"),\n            ParamSpec::continuous("v_size", "Vertex Size", 0.1, 4.0, 1.0, "F1", "vsize"),\n            ParamSpec::toggle("anim", "Animate", 0.0, 1.0, 0.0, "anim"),\n            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),\n            ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.1, "F2", "window"),\n            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),\n        ],\n        string_params: &[],\n    }\n}\n\n// ── Lissajous ',
    ),
    Rename(
        desc="Lissajous: Verts → Vertices, Anim → Animate (scoped after `phase` line)",
        file=gen_metadata(),
        old='ParamSpec::continuous("phase", "Phase", 0.0, 2.0, 0.0, "F2", "phase"),\n            ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.002, "F4", "line"),\n            ParamSpec::toggle("verts", "Verts", 0.0, 1.0, 0.0, "verts"),\n            ParamSpec::continuous("v_size", "VSize", 0.1, 4.0, 0.5, "F1", "vsize"),\n            ParamSpec::toggle("anim", "Anim", 0.0, 1.0, 1.0, "anim"),',
        new='ParamSpec::continuous("phase", "Phase", 0.0, 2.0, 0.0, "F2", "phase"),\n            ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.002, "F4", "line"),\n            ParamSpec::toggle("verts", "Vertices", 0.0, 1.0, 0.0, "verts"),\n            ParamSpec::continuous("v_size", "Vertex Size", 0.1, 4.0, 0.5, "F1", "vsize"),\n            ParamSpec::toggle("anim", "Animate", 0.0, 1.0, 1.0, "anim"),',
    ),
    Rename(
        desc="Oscilloscope XY: Verts → Vertices, Anim → Animate (scoped after `line` line)",
        file=gen_metadata(),
        old='ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.002, "F4", "line"),\n            ParamSpec::toggle("verts", "Verts", 0.0, 1.0, 0.0, "verts"),\n            ParamSpec::continuous("v_size", "VSize", 0.1, 4.0, 0.5, "F1", "vsize"),\n            ParamSpec::toggle("anim", "Anim", 0.0, 1.0, 1.0, "anim"),\n            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.63, "F1", "speed"),',
        new='ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.002, "F4", "line"),\n            ParamSpec::toggle("verts", "Vertices", 0.0, 1.0, 0.0, "verts"),\n            ParamSpec::continuous("v_size", "Vertex Size", 0.1, 4.0, 0.5, "F1", "vsize"),\n            ParamSpec::toggle("anim", "Animate", 0.0, 1.0, 1.0, "anim"),\n            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.63, "F1", "speed"),',
    ),
    Rename(
        desc="Wireframe Zoo: Verts → Vertices (scoped after `shape` line)",
        file=gen_metadata(),
        old='ParamSpec::whole_labels("shape", "Shape", 0.0, 4.0, 0.0, &["Tetra","Cube","Octa","Icosa","Dodeca"], "shape"),\n            ParamSpec::toggle("verts", "Verts", 0.0, 1.0, 1.0, "verts"),\n            ParamSpec::continuous("v_size", "VSize", 0.1, 4.0, 1.0, "F1", "vsize"),',
        new='ParamSpec::whole_labels("shape", "Shape", 0.0, 4.0, 0.0, &["Tetra","Cube","Octa","Icosa","Dodeca"], "shape"),\n            ParamSpec::toggle("verts", "Vertices", 0.0, 1.0, 1.0, "verts"),\n            ParamSpec::continuous("v_size", "Vertex Size", 0.1, 4.0, 1.0, "F1", "vsize"),',
    ),

    # ── Dist → Distance (Tesseract + Duocylinder) ───────────────────────
    # Both generators have `dist` param with same range/default; disambiguate
    # by the unique `line` default (Tesseract 0.002, Duocylinder 0.0015).
    Rename(
        desc="Tesseract: Dist → Distance (scoped by Tesseract's line=0.002)",
        file=gen_metadata(),
        old='ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.002, "F4", "line"),\n            ParamSpec::continuous("dist", "Dist", 1.0, 6.0, 3.0, "F1", "dist"),',
        new='ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.002, "F4", "line"),\n            ParamSpec::continuous("dist", "Distance", 1.0, 6.0, 3.0, "F1", "dist"),',
    ),
    Rename(
        desc="Duocylinder: Dist → Distance (scoped by Duocylinder's line=0.0015)",
        file=gen_metadata(),
        old='ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.0015, "F4", "line"),\n            ParamSpec::continuous("dist", "Dist", 1.0, 6.0, 3.0, "F1", "dist"),',
        new='ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.0015, "F4", "line"),\n            ParamSpec::continuous("dist", "Distance", 1.0, 6.0, 3.0, "F1", "dist"),',
    ),

    # ── Cam * → Camera * (Galactic Rock, Metallic Glass, Digital Plants — 4 params each) ─
    Rename(
        desc="Galactic Rock: Cam Dist/Orbit/Tilt/FOV → Camera *",
        file=gen_metadata(),
        old='ParamSpec::continuous("cam_dist", "Cam Dist", 0.1, 10.0, 0.8, "F2", "camDist"),\n            ParamSpec::continuous("cam_orbit", "Cam Orbit", -180.0, 180.0, 0.0, "F0", "camOrbit"),\n            ParamSpec::continuous("cam_tilt", "Cam Tilt", -90.0, 90.0, 10.0, "F0", "camTilt"),\n            ParamSpec::continuous("cam_fov", "Cam FOV", 20.0, 120.0, 60.0, "F0", "camFov"),',
        new='ParamSpec::continuous("cam_dist", "Camera Distance", 0.1, 10.0, 0.8, "F2", "camDist"),\n            ParamSpec::continuous("cam_orbit", "Camera Orbit", -180.0, 180.0, 0.0, "F0", "camOrbit"),\n            ParamSpec::continuous("cam_tilt", "Camera Tilt", -90.0, 90.0, 10.0, "F0", "camTilt"),\n            ParamSpec::continuous("cam_fov", "Camera FOV", 20.0, 120.0, 60.0, "F0", "camFov"),',
    ),
    Rename(
        desc="Metallic Glass: Cam Dist/Orbit/Tilt/FOV → Camera *",
        file=gen_metadata(),
        old='ParamSpec::continuous("cam_dist", "Cam Dist", 0.5, 10.0, 2.5, "F2", "camDist"),\n            ParamSpec::continuous("cam_orbit", "Cam Orbit", -180.0, 180.0, 0.0, "F0", "camOrbit"),\n            ParamSpec::continuous("cam_tilt", "Cam Tilt", -90.0, 90.0, -10.0, "F0", "camTilt"),\n            ParamSpec::continuous("cam_fov", "Cam FOV", 20.0, 120.0, 54.0, "F0", "camFov"),',
        new='ParamSpec::continuous("cam_dist", "Camera Distance", 0.5, 10.0, 2.5, "F2", "camDist"),\n            ParamSpec::continuous("cam_orbit", "Camera Orbit", -180.0, 180.0, 0.0, "F0", "camOrbit"),\n            ParamSpec::continuous("cam_tilt", "Camera Tilt", -90.0, 90.0, -10.0, "F0", "camTilt"),\n            ParamSpec::continuous("cam_fov", "Camera FOV", 20.0, 120.0, 54.0, "F0", "camFov"),',
    ),
    Rename(
        desc="Digital Plants: Cam Dist/Orbit/Tilt/FOV → Camera *",
        file=gen_metadata(),
        old='ParamSpec::continuous("cam_dist", "Cam Dist", 0.5, 10.0, 3.5, "F1", "camDist"),\n            ParamSpec::continuous("cam_orbit", "Cam Orbit", -180.0, 180.0, 0.0, "F0", "camOrbit"),\n            ParamSpec::continuous("cam_tilt", "Cam Tilt", -90.0, 90.0, 15.0, "F0", "camTilt"),\n            ParamSpec::continuous("cam_fov", "Cam FOV", 20.0, 120.0, 50.0, "F0", "camFov"),',
        new='ParamSpec::continuous("cam_dist", "Camera Distance", 0.5, 10.0, 3.5, "F1", "camDist"),\n            ParamSpec::continuous("cam_orbit", "Camera Orbit", -180.0, 180.0, 0.0, "F0", "camOrbit"),\n            ParamSpec::continuous("cam_tilt", "Camera Tilt", -90.0, 90.0, 15.0, "F0", "camTilt"),\n            ParamSpec::continuous("cam_fov", "Camera FOV", 20.0, 120.0, 50.0, "F0", "camFov"),',
    ),

    # ── Black Hole: Cam Dist + Cam Velocity → Camera Distance / Camera Velocity ──
    Rename(
        desc="Black Hole: Cam Dist → Camera Distance",
        file=gen_metadata(),
        old='ParamSpec::continuous("cam_dist", "Cam Dist", 0.1, 50.0, 20.0, "F1", "camDist"),',
        new='ParamSpec::continuous("cam_dist", "Camera Distance", 0.1, 50.0, 20.0, "F1", "camDist"),',
    ),
    Rename(
        desc="Black Hole: Cam Velocity → Camera Velocity",
        file=gen_metadata(),
        old='ParamSpec::continuous("cam_velocity", "Cam Velocity", 0.0, 0.99, 0.0, "F2", "camVelocity"),',
        new='ParamSpec::continuous("cam_velocity", "Camera Velocity", 0.0, 0.99, 0.0, "F2", "camVelocity"),',
    ),

    # ── Mycelium: SensDist/SensAngle → Sensor Distance/Sensor Angle ─────
    Rename(
        desc="Mycelium: SensDist → Sensor Distance",
        file=gen_metadata(),
        old='ParamSpec::continuous("sens_dist", "SensDist", 0.005, 0.1, 0.02, "F3", "sensdist"),',
        new='ParamSpec::continuous("sens_dist", "Sensor Distance", 0.005, 0.1, 0.02, "F3", "sensdist"),',
    ),
    Rename(
        desc="Mycelium: SensAngle → Sensor Angle",
        file=gen_metadata(),
        old='ParamSpec::continuous("sens_angle", "SensAngle", 0.1, 1.5, 0.8, "F2", "sensangle"),',
        new='ParamSpec::continuous("sens_angle", "Sensor Angle", 0.1, 1.5, 0.8, "F2", "sensangle"),',
    ),

    # ── Oily Fluid: VelDamp/VelDisp/ColDisp/Sat/Bright → full names ─────
    Rename(
        desc="Oily Fluid: VelDamp → Velocity Damp",
        file=gen_metadata(),
        old='ParamSpec::continuous("vel_damp", "VelDamp", 0.85, 0.999, 0.98, "F3", "veldamp"),',
        new='ParamSpec::continuous("vel_damp", "Velocity Damp", 0.85, 0.999, 0.98, "F3", "veldamp"),',
    ),
    Rename(
        desc="Oily Fluid: Sat → Saturation",
        file=gen_metadata(),
        old='ParamSpec::continuous("sat", "Sat", 0.0, 2.0, 1.0, "F2", "sat"),',
        new='ParamSpec::continuous("sat", "Saturation", 0.0, 2.0, 1.0, "F2", "sat"),',
    ),
    Rename(
        desc="Oily Fluid: Bright → Brightness",
        file=gen_metadata(),
        old='ParamSpec::continuous("bright", "Bright", 0.0, 2.0, 1.0, "F2", "bright"),',
        new='ParamSpec::continuous("bright", "Brightness", 0.0, 2.0, 1.0, "F2", "bright"),',
    ),
    Rename(
        desc="Oily Fluid: VelDisp → Velocity Displace",
        file=gen_metadata(),
        old='ParamSpec::continuous("vel_disp", "VelDisp", 0.1, 10.0, 1.0, "F2", "velDisp"),',
        new='ParamSpec::continuous("vel_disp", "Velocity Displace", 0.1, 10.0, 1.0, "F2", "velDisp"),',
    ),
    Rename(
        desc="Oily Fluid: ColDisp → Color Displace",
        file=gen_metadata(),
        old='ParamSpec::continuous("col_disp", "ColDisp", 0.1, 10.0, 1.0, "F2", "colDisp"),',
        new='ParamSpec::continuous("col_disp", "Color Displace", 0.1, 10.0, 1.0, "F2", "colDisp"),',
    ),

    # ── Galactic Rock: Wave Amp/Wave Freq → full names, Light Int → Light Intensity ──
    Rename(
        desc="Galactic Rock: Wave Amp → Wave Amplitude",
        file=gen_metadata(),
        old='ParamSpec::continuous("wave_amp", "Wave Amp", 0.0, 0.5, 0.1, "F3", "waveAmp"),',
        new='ParamSpec::continuous("wave_amp", "Wave Amplitude", 0.0, 0.5, 0.1, "F3", "waveAmp"),',
    ),
    Rename(
        desc="Galactic Rock: Wave Freq → Wave Frequency",
        file=gen_metadata(),
        old='ParamSpec::continuous("wave_freq", "Wave Freq", 0.1, 2.0, 0.5, "F2", "waveFreq"),',
        new='ParamSpec::continuous("wave_freq", "Wave Frequency", 0.1, 2.0, 0.5, "F2", "waveFreq"),',
    ),
    Rename(
        desc="Galactic Rock: Light Int → Light Intensity",
        file=gen_metadata(),
        old='ParamSpec::continuous("light_int", "Light Int", 0.1, 10.0, 2.5, "F1", "lightInt"),',
        new='ParamSpec::continuous("light_int", "Light Intensity", 0.1, 10.0, 2.5, "F1", "lightInt"),',
    ),

    # ── Metallic Glass: Light Int → Light Intensity, Edge Str → Edge Strength ──
    Rename(
        desc="Metallic Glass: Edge Str → Edge Strength",
        file=gen_metadata(),
        old='ParamSpec::continuous("edge_str", "Edge Str", 0.5, 20.0, 5.0, "F1", "edgeStr"),',
        new='ParamSpec::continuous("edge_str", "Edge Strength", 0.5, 20.0, 5.0, "F1", "edgeStr"),',
    ),
    Rename(
        desc="Metallic Glass: Light Int → Light Intensity",
        file=gen_metadata(),
        old='ParamSpec::continuous("light_int", "Light Int", 0.1, 10.0, 3.5, "F1", "lightInt"),',
        new='ParamSpec::continuous("light_int", "Light Intensity", 0.1, 10.0, 3.5, "F1", "lightInt"),',
    ),

    # ── Fluid Sim 3D: Ctr Scale → Container Scale, Vol Res → Volume Resolution ──
    Rename(
        desc="Fluid Sim 3D: Ctr Scale → Container Scale",
        file=gen_metadata(),
        old='ParamSpec::continuous("ctr_scale", "Ctr Scale", 0.2, 1.0, 0.8, "F2", "containerScale"),',
        new='ParamSpec::continuous("ctr_scale", "Container Scale", 0.2, 1.0, 0.8, "F2", "containerScale"),',
    ),
    Rename(
        desc="Fluid Sim 3D: Vol Res → Volume Resolution",
        file=gen_metadata(),
        old='ParamSpec::whole_labels("vol_res", "Vol Res", 0.0, 2.0, 0.0, &["64", "128", "256"], "volumeRes"),',
        new='ParamSpec::whole_labels("vol_res", "Volume Resolution", 0.0, 2.0, 0.0, &["64", "128", "256"], "volumeRes"),',
    ),

    # ── Strange Attractor: Type → Attractor Type ────────────────────────
    Rename(
        desc="Strange Attractor: Type → Attractor Type",
        file=generator("strange_attractor"),
        old='ParamSpec::whole_labels("type", "Type", 0.0, 4.0, 0.0, &["Lorenz", "Rossler", "Aizawa", "Thomas", "Halvorsen"], "type"),',
        new='ParamSpec::whole_labels("type", "Attractor Type", 0.0, 4.0, 0.0, &["Lorenz", "Rossler", "Aizawa", "Thomas", "Halvorsen"], "type"),',
    ),

    # ── Digital Plants: Anim Speed/Petal Amp/Rot Speed → full names ─────
    Rename(
        desc="Digital Plants: Anim Speed → Animation Speed",
        file=gen_metadata(),
        old='ParamSpec::continuous("anim_speed", "Anim Speed", 0.0, 1.0, 0.5, "F2", "animSpeed"),',
        new='ParamSpec::continuous("anim_speed", "Animation Speed", 0.0, 1.0, 0.5, "F2", "animSpeed"),',
    ),
    Rename(
        desc="Digital Plants: Petal Amp → Petal Amplitude",
        file=gen_metadata(),
        old='ParamSpec::continuous("petal_amp", "Petal Amp", 0.0, 80.0, 60.0, "F0", "petalAmp"),',
        new='ParamSpec::continuous("petal_amp", "Petal Amplitude", 0.0, 80.0, 60.0, "F0", "petalAmp"),',
    ),
    Rename(
        desc="Digital Plants: Rot Speed → Rotation Speed",
        file=gen_metadata(),
        old='ParamSpec::continuous("rot_speed", "Rot Speed", 0.0, 3.0, 0.3, "F2", "rotSpeed"),',
        new='ParamSpec::continuous("rot_speed", "Rotation Speed", 0.0, 3.0, 0.3, "F2", "rotSpeed"),',
    ),

    # ── Display name fixes ──────────────────────────────────────────────
    Rename(
        desc="Fluid Sim 3D display name: Fluid Sim 3D → Fluid Simulation 3D",
        file=gen_metadata(),
        old='display_name: "Fluid Sim 3D",',
        new='display_name: "Fluid Simulation 3D",',
    ),
    Rename(
        desc="Basic Shapes Snap display name: Basic Shapes Snap → Basic Shapes",
        file=gen_metadata(),
        old='display_name: "Basic Shapes Snap",',
        new='display_name: "Basic Shapes",',
    ),

    # ========================================================================
    # Phase 6 — `Count (M)` redesign (§9.3.2 / UX call 6).
    #
    # 4 generators have a slider labeled `Count (M)` where `(M)` is a fake
    # unit smuggled into the label, meaning "value × 1e6 particles". The
    # value semantic doesn't change (still 0.1–8.0 = 0.1M–8M particles);
    # we just stop pretending the parens are part of the name and put `M`
    # in the actual unit field where the slider can render it cleanly.
    #
    # Param id stays `count_m` to avoid an alias migration. The label is
    # what the user reads; id is internal.
    # ========================================================================

    # Three generators in gen_metadata.rs share the same count_m line;
    # disambiguate by the unique trailing param of each block.
    Rename(
        desc="Fluid Simulation: Count (M) → Particle Count (scoped by trailing `fill` param)",
        file=gen_metadata(),
        old='ParamSpec::continuous("count_m", "Count (M)", 0.1, 8.0, 2.0, "F1", "count"),\n            ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 0.0, "snap"),\n            ParamSpec::whole_labels("snap_mode", "Snap Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "snapMode"),\n            ParamSpec::continuous("size", "Size", 1.0, 8.0, 3.0, "F1", "size"),\n            ParamSpec::continuous("anti_clump", "Anti-Clump", 0.0, 60.0, 20.0, "F0", "antiClump"),\n            ParamSpec::continuous("force", "Force", 0.0, 0.1, 0.005, "F3", "force"),\n            ParamSpec::continuous("fill", "Fill", 0.0, 1.0, 1.0, "F2", "fill"),',
        new='ParamSpec::continuous("count_m", "Particle Count", 0.1, 8.0, 2.0, "F1", "M"),\n            ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 0.0, "snap"),\n            ParamSpec::whole_labels("snap_mode", "Snap Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "snapMode"),\n            ParamSpec::continuous("size", "Size", 1.0, 8.0, 3.0, "F1", "size"),\n            ParamSpec::continuous("anti_clump", "Anti-Clump", 0.0, 60.0, 20.0, "F0", "antiClump"),\n            ParamSpec::continuous("force", "Force", 0.0, 0.1, 0.005, "F3", "force"),\n            ParamSpec::continuous("fill", "Fill", 0.0, 1.0, 1.0, "F2", "fill"),',
    ),
    Rename(
        desc="Fluid Simulation 3D: Count (M) → Particle Count (scoped by trailing `container` param)",
        file=gen_metadata(),
        old='ParamSpec::continuous("count_m", "Count (M)", 0.1, 8.0, 2.0, "F1", "count"),\n            ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 0.0, "snap"),\n            ParamSpec::whole_labels("snap_mode", "Snap Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "snapMode"),\n            ParamSpec::continuous("size", "Size", 1.0, 8.0, 3.0, "F1", "size"),\n            ParamSpec::continuous("anti_clump", "Anti-Clump", 0.0, 60.0, 20.0, "F0", "antiClump"),\n            ParamSpec::continuous("force", "Force", 0.0, 0.1, 0.005, "F3", "force"),\n            ParamSpec::whole_labels("container", "Container", 0.0, 3.0, 0.0, &["None", "Cube", "Sphere", "Torus"], "container"),',
        new='ParamSpec::continuous("count_m", "Particle Count", 0.1, 8.0, 2.0, "F1", "M"),\n            ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 0.0, "snap"),\n            ParamSpec::whole_labels("snap_mode", "Snap Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "snapMode"),\n            ParamSpec::continuous("size", "Size", 1.0, 8.0, 3.0, "F1", "size"),\n            ParamSpec::continuous("anti_clump", "Anti-Clump", 0.0, 60.0, 20.0, "F0", "antiClump"),\n            ParamSpec::continuous("force", "Force", 0.0, 0.1, 0.005, "F3", "force"),\n            ParamSpec::whole_labels("container", "Container", 0.0, 3.0, 0.0, &["None", "Cube", "Sphere", "Torus"], "container"),',
    ),
    Rename(
        desc="Particle Text: Count (M) → Particle Count (scoped by trailing `text_size` param)",
        file=gen_metadata(),
        old='ParamSpec::continuous("count_m", "Count (M)", 0.1, 8.0, 2.0, "F1", "count"),\n            ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 0.0, "snap"),\n            ParamSpec::whole_labels("snap_mode", "Snap Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "snapMode"),\n            ParamSpec::continuous("size", "Size", 1.0, 8.0, 3.0, "F1", "size"),\n            ParamSpec::continuous("anti_clump", "Anti-Clump", 0.0, 60.0, 20.0, "F0", "antiClump"),\n            ParamSpec::continuous("force", "Force", 0.0, 0.1, 0.005, "F3", "force"),\n            ParamSpec::continuous("text_size", "Text Size", 0.05, 1.0, 0.25, "F2", "textSize"),',
        new='ParamSpec::continuous("count_m", "Particle Count", 0.1, 8.0, 2.0, "F1", "M"),\n            ParamSpec::toggle("snap", "Snap", 0.0, 1.0, 0.0, "snap"),\n            ParamSpec::whole_labels("snap_mode", "Snap Mode", 0.0, 4.0, 0.0, &["Turbulence", "Rot Flip", "Flow Inv", "Pattern", "Inject"], "snapMode"),\n            ParamSpec::continuous("size", "Size", 1.0, 8.0, 3.0, "F1", "size"),\n            ParamSpec::continuous("anti_clump", "Anti-Clump", 0.0, 60.0, 20.0, "F0", "antiClump"),\n            ParamSpec::continuous("force", "Force", 0.0, 0.1, 0.005, "F3", "force"),\n            ParamSpec::continuous("text_size", "Text Size", 0.05, 1.0, 0.25, "F2", "textSize"),',
    ),
    Rename(
        desc="Strange Attractor: Count (M) → Particle Count, unit M",
        file=generator("strange_attractor"),
        old='ParamSpec::continuous("count_m", "Count (M)", 0.1, 2.0, 0.5, "F1", "count"),',
        new='ParamSpec::continuous("count_m", "Particle Count", 0.1, 2.0, 0.5, "F1", "M"),',
    ),

    # ========================================================================
    # Phase 7a — Param id renames where outer and inner already align.
    #
    # Each id rename touches 4 sites per effect:
    #   1. Import: `EffectMetadata` → `{EffectAliasMetadata, EffectMetadata}`
    #   2. New inventory submission for `EffectAliasMetadata` with the rename
    #      mapping. Saved projects with the old id keep loading.
    #   3. ParamSpec id (in EffectMetadata.params).
    #   4. ParamBinding id (in atomic_chain_spec! bindings).
    #
    # The OSC suffix (last ParamSpec arg) is separate from the id — OSC
    # paths use the suffix, not the id, so this rename is OSC-safe.
    # ========================================================================

    # ── Kaleidoscope: segs → segments ──────────────────────────────────
    Rename(
        desc="Kaleidoscope: add EffectAliasMetadata import",
        file=effect("kaleidoscope"),
        old='use manifold_core::effect_registration::EffectMetadata;',
        new='use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};',
    ),
    Rename(
        desc="Kaleidoscope: insert EffectAliasMetadata submission",
        file=effect("kaleidoscope"),
        old='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::KALEIDOSCOPE,
        create: |device| Box::new(KaleidoscopeFX::new(device)),
    }
}''',
        new='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::KALEIDOSCOPE,
        create: |device| Box::new(KaleidoscopeFX::new(device)),
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::KALEIDOSCOPE,
        aliases: &[("segs", Some("segments"))],
    }
}''',
    ),
    Rename(
        desc="Kaleidoscope: ParamSpec id segs → segments",
        file=effect("kaleidoscope"),
        old='ParamSpec::whole("segs", "Segments", 2.0, 16.0, 6.0, "Segments"),',
        new='ParamSpec::whole("segments", "Segments", 2.0, 16.0, 6.0, "Segments"),',
    ),
    Rename(
        desc="Kaleidoscope: ParamBinding id segs → segments",
        file=effect("kaleidoscope"),
        old='id: Cow::Borrowed("segs"),',
        new='id: Cow::Borrowed("segments"),',
    ),

    # ── Glitch: block → block_size ─────────────────────────────────────
    Rename(
        desc="Glitch: add EffectAliasMetadata import",
        file=effect("glitch"),
        old='use manifold_core::effect_registration::EffectMetadata;',
        new='use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};',
    ),
    Rename(
        desc="Glitch: insert EffectAliasMetadata submission",
        file=effect("glitch"),
        old='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::GLITCH,
        create: |device| Box::new(GlitchFX::new(device)),
    }
}''',
        new='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::GLITCH,
        create: |device| Box::new(GlitchFX::new(device)),
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::GLITCH,
        aliases: &[("block", Some("block_size"))],
    }
}''',
    ),
    Rename(
        desc="Glitch: ParamSpec id block → block_size",
        file=effect("glitch"),
        old='ParamSpec::continuous("block", "Block Size", 4.0, 64.0, 16.0, "F2", "BlockSize"),',
        new='ParamSpec::continuous("block_size", "Block Size", 4.0, 64.0, 16.0, "F2", "BlockSize"),',
    ),
    Rename(
        desc="Glitch: ParamBinding id block → block_size",
        file=effect("glitch"),
        old='id: Cow::Borrowed("block"),',
        new='id: Cow::Borrowed("block_size"),',
    ),

    # ── Transform: rot → rotation ──────────────────────────────────────
    Rename(
        desc="Transform: add EffectAliasMetadata import",
        file=effect("transform"),
        old='use manifold_core::effect_registration::EffectMetadata;',
        new='use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};',
    ),
    Rename(
        desc="Transform: insert EffectAliasMetadata submission",
        file=effect("transform"),
        old='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::TRANSFORM,
        create: |device| Box::new(TransformFX::new(device)),
    }
}''',
        new='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::TRANSFORM,
        create: |device| Box::new(TransformFX::new(device)),
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::TRANSFORM,
        aliases: &[("rot", Some("rotation"))],
    }
}''',
    ),
    Rename(
        desc="Transform: ParamSpec id rot → rotation",
        file=effect("transform"),
        old='ParamSpec::continuous("rot", "Rotation", -180.0, 180.0, 0.0, "F2", ""),',
        new='ParamSpec::continuous("rotation", "Rotation", -180.0, 180.0, 0.0, "F2", ""),',
    ),
    Rename(
        desc="Transform: ParamBinding id rot → rotation",
        file=effect("transform"),
        old='id: Cow::Borrowed("rot"),',
        new='id: Cow::Borrowed("rotation"),',
    ),

    # ========================================================================
    # Phase 7b — Param id renames with no inner-primitive impact.
    # Same 4-edit pattern as 7a per effect. Multi-param renames bundle their
    # aliases into one EffectAliasMetadata submission.
    # ========================================================================

    # ── Dither: algo → pattern ─────────────────────────────────────────
    Rename(
        desc="Dither: add EffectAliasMetadata import",
        file=effect("dither"),
        old='use manifold_core::effect_registration::EffectMetadata;',
        new='use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};',
    ),
    Rename(
        desc="Dither: insert EffectAliasMetadata submission",
        file=effect("dither"),
        old='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::DITHER,
        create: |device| Box::new(DitherFX::new(device)),
    }
}''',
        new='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::DITHER,
        create: |device| Box::new(DitherFX::new(device)),
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::DITHER,
        aliases: &[("algo", Some("pattern"))],
    }
}''',
    ),
    Rename(
        desc="Dither: ParamSpec id algo → pattern",
        file=effect("dither"),
        old='ParamSpec::whole_labels("algo", "Pattern", 0.0, 5.0, 0.0, &["Bayer", "Halftone", "Lines", "X-Hatch", "Noise", "Diamond"], "Algorithm"),',
        new='ParamSpec::whole_labels("pattern", "Pattern", 0.0, 5.0, 0.0, &["Bayer", "Halftone", "Lines", "X-Hatch", "Noise", "Diamond"], "Algorithm"),',
    ),
    Rename(
        desc="Dither: ParamBinding id algo → pattern",
        file=effect("dither"),
        old='id: Cow::Borrowed("algo"),',
        new='id: Cow::Borrowed("pattern"),',
    ),

    # ── HDR Boost: thresh → threshold ──────────────────────────────────
    Rename(
        desc="HDR Boost: add EffectAliasMetadata import",
        file=effect("hdr_boost"),
        old='use manifold_core::effect_registration::EffectMetadata;',
        new='use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};',
    ),
    Rename(
        desc="HDR Boost: insert EffectAliasMetadata submission",
        file=effect("hdr_boost"),
        old='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::HDR_BOOST,
        create: |device| Box::new(HdrBoostFX::new(device)),
    }
}''',
        new='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::HDR_BOOST,
        create: |device| Box::new(HdrBoostFX::new(device)),
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::HDR_BOOST,
        aliases: &[("thresh", Some("threshold"))],
    }
}''',
    ),
    Rename(
        desc="HDR Boost: ParamSpec id thresh → threshold",
        file=effect("hdr_boost"),
        old='ParamSpec::continuous("thresh", "Threshold", 0.0, 1.0, 0.15, "F2", "Threshold"),',
        new='ParamSpec::continuous("threshold", "Threshold", 0.0, 1.0, 0.15, "F2", "Threshold"),',
    ),
    Rename(
        desc="HDR Boost: ParamBinding id thresh → threshold",
        file=effect("hdr_boost"),
        old='id: Cow::Borrowed("thresh"),',
        new='id: Cow::Borrowed("threshold"),',
    ),

    # ── Edge Detect: thresh → threshold ─────────────────────────────────
    Rename(
        desc="Edge Detect: add EffectAliasMetadata import",
        file=effect("edge_detect"),
        old='use manifold_core::effect_registration::EffectMetadata;',
        new='use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};',
    ),
    Rename(
        desc="Edge Detect: insert EffectAliasMetadata submission",
        file=effect("edge_detect"),
        old='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::EDGE_DETECT,
        create: |device| Box::new(EdgeDetectFX::new(device)),
    }
}''',
        new='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::EDGE_DETECT,
        create: |device| Box::new(EdgeDetectFX::new(device)),
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::EDGE_DETECT,
        aliases: &[("thresh", Some("threshold"))],
    }
}''',
    ),
    Rename(
        desc="Edge Detect: ParamSpec id thresh → threshold",
        file=effect("edge_detect"),
        old='ParamSpec::continuous("thresh", "Threshold", 0.0, 1.0, 0.1, "F2", "Threshold"),',
        new='ParamSpec::continuous("threshold", "Threshold", 0.0, 1.0, 0.1, "F2", "Threshold"),',
    ),
    Rename(
        desc="Edge Detect: ParamBinding id thresh → threshold",
        file=effect("edge_detect"),
        old='id: Cow::Borrowed("thresh"),',
        new='id: Cow::Borrowed("threshold"),',
    ),

    # ── Halation: thresh → threshold, sat → saturation ──────────────────
    Rename(
        desc="Halation: add EffectAliasMetadata import",
        file=effect("halation"),
        old='use manifold_core::effect_registration::EffectMetadata;',
        new='use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};',
    ),
    Rename(
        desc="Halation: insert EffectAliasMetadata submission",
        file=effect("halation"),
        old='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::HALATION,
        create: |device| Box::new(HalationFX::new(device)),
    }
}''',
        new='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::HALATION,
        create: |device| Box::new(HalationFX::new(device)),
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::HALATION,
        aliases: &[
            ("thresh", Some("threshold")),
            ("sat", Some("saturation")),
        ],
    }
}''',
    ),
    Rename(
        desc="Halation: ParamSpec id thresh → threshold",
        file=effect("halation"),
        old='ParamSpec::continuous("thresh", "Threshold", 0.0, 1.0, 0.5, "F2", "Threshold"),',
        new='ParamSpec::continuous("threshold", "Threshold", 0.0, 1.0, 0.5, "F2", "Threshold"),',
    ),
    Rename(
        desc="Halation: ParamSpec id sat → saturation",
        file=effect("halation"),
        old='ParamSpec::continuous("sat", "Saturation", 0.0, 1.0, 0.6, "F2", "Saturation"),',
        new='ParamSpec::continuous("saturation", "Saturation", 0.0, 1.0, 0.6, "F2", "Saturation"),',
    ),
    Rename(
        desc="Halation: ParamBinding id thresh → threshold",
        file=effect("halation"),
        old='id: Cow::Borrowed("thresh"),',
        new='id: Cow::Borrowed("threshold"),',
    ),
    Rename(
        desc="Halation: ParamBinding id sat → saturation",
        file=effect("halation"),
        old='id: Cow::Borrowed("sat"),',
        new='id: Cow::Borrowed("saturation"),',
    ),

    # ── Color Grade: sat → saturation, tint_sat → tint_saturation, focus → tint_focus ──
    Rename(
        desc="Color Grade: add EffectAliasMetadata import",
        file=effect("color_grade"),
        old='use manifold_core::effect_registration::EffectMetadata;',
        new='use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};',
    ),
    Rename(
        desc="Color Grade: insert EffectAliasMetadata submission",
        file=effect("color_grade"),
        old='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::COLOR_GRADE,
        create: |device| Box::new(ColorGradeFX::new(device)),
    }
}''',
        new='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::COLOR_GRADE,
        create: |device| Box::new(ColorGradeFX::new(device)),
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::COLOR_GRADE,
        aliases: &[
            ("sat", Some("saturation")),
            ("tint_sat", Some("tint_saturation")),
            ("focus", Some("tint_focus")),
        ],
    }
}''',
    ),
    Rename(
        desc="Color Grade: ParamSpec id sat → saturation",
        file=effect("color_grade"),
        old='ParamSpec::continuous("sat", "Saturation", 0.0, 2.0, 1.0, "F2", "Saturation"),',
        new='ParamSpec::continuous("saturation", "Saturation", 0.0, 2.0, 1.0, "F2", "Saturation"),',
    ),
    Rename(
        desc="Color Grade: ParamSpec id tint_sat → tint_saturation",
        file=effect("color_grade"),
        old='ParamSpec::continuous("tint_sat", "Tint Saturation", 0.0, 2.0, 1.0, "F2", "TintSaturation"),',
        new='ParamSpec::continuous("tint_saturation", "Tint Saturation", 0.0, 2.0, 1.0, "F2", "TintSaturation"),',
    ),
    Rename(
        desc="Color Grade: ParamSpec id focus → tint_focus",
        file=effect("color_grade"),
        old='ParamSpec::continuous("focus", "Tint Focus", 0.0, 1.0, 0.75, "F2", "ColorizeFocus"),',
        new='ParamSpec::continuous("tint_focus", "Tint Focus", 0.0, 1.0, 0.75, "F2", "ColorizeFocus"),',
    ),
    Rename(
        desc="Color Grade: ParamBinding id sat → saturation",
        file=effect("color_grade"),
        old='id: Cow::Borrowed("sat"),',
        new='id: Cow::Borrowed("saturation"),',
    ),
    Rename(
        desc="Color Grade: ParamBinding id tint_sat → tint_saturation",
        file=effect("color_grade"),
        old='id: Cow::Borrowed("tint_sat"),',
        new='id: Cow::Borrowed("tint_saturation"),',
    ),
    Rename(
        desc="Color Grade: ParamBinding id focus → tint_focus",
        file=effect("color_grade"),
        old='id: Cow::Borrowed("focus"),',
        new='id: Cow::Borrowed("tint_focus"),',
    ),

    # ── Wireframe Depth: wire_res → wire_resolution ─────────────────────
    Rename(
        desc="Wireframe Depth: add EffectAliasMetadata import",
        file=effect("wireframe_depth"),
        old='use manifold_core::effect_registration::EffectMetadata;',
        new='use manifold_core::effect_registration::{EffectAliasMetadata, EffectMetadata};',
    ),
    Rename(
        desc="Wireframe Depth: insert EffectAliasMetadata submission",
        file=effect("wireframe_depth"),
        old='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::WIREFRAME_DEPTH,
        create: |device| Box::new(WireframeDepthFX::new(device)),
    }
}''',
        new='''inventory::submit! {
    EffectFactory {
        id: EffectTypeId::WIREFRAME_DEPTH,
        create: |device| Box::new(WireframeDepthFX::new(device)),
    }
}

inventory::submit! {
    EffectAliasMetadata {
        id: EffectTypeId::WIREFRAME_DEPTH,
        aliases: &[("wire_res", Some("wire_resolution"))],
    }
}''',
    ),
    Rename(
        desc="Wireframe Depth: ParamSpec id wire_res → wire_resolution",
        file=effect("wireframe_depth"),
        old='ParamSpec::continuous("wire_res", "Wire Resolution", 0.5, 1.0, 1.0, "F2", "WireRes"),',
        new='ParamSpec::continuous("wire_resolution", "Wire Resolution", 0.5, 1.0, 1.0, "F2", "WireRes"),',
    ),
    Rename(
        desc="Wireframe Depth: ParamBinding id wire_res → wire_resolution",
        file=effect("wireframe_depth"),
        old='id: Cow::Borrowed("wire_res"),',
        new='id: Cow::Borrowed("wire_resolution"),',
    ),
]


# ============================================================================
# Apply
# ============================================================================


def apply_one(r: Rename) -> str:
    """Return 'applied' | 'skip-applied' | 'error'."""
    text = r.file.read_text()
    count = text.count(r.old)
    if count == 0:
        # Either already applied (idempotent) or the find string never matched.
        if r.new and r.new in text:
            return "skip-applied"
        return "error-not-found"
    if count > 1:
        return f"error-multiple-{count}"
    if not DRY_RUN:
        r.file.write_text(text.replace(r.old, r.new, 1))
    return "applied"


def main() -> int:
    if not RENAMES:
        print("No renames defined yet. Add entries to RENAMES.")
        return 0

    applied = skipped = errors = 0
    for r in RENAMES:
        status = apply_one(r)
        if status == "applied":
            applied += 1
            print(f"  [{'dry' if DRY_RUN else 'apply'}] {r.desc}")
        elif status == "skip-applied":
            skipped += 1
            print(f"  [already-applied] {r.desc}")
        else:
            errors += 1
            print(f"  [ERROR:{status}] {r.desc}")
            print(f"    file: {r.file.relative_to(REPO_ROOT)}")
            print(f"    old:  {r.old!r}")

    print()
    print(f"Summary: {applied} applied, {skipped} already-applied, {errors} errors.")
    if errors:
        return 1

    if DRY_RUN or NO_REGEN:
        return 0

    print("\nRegenerating bundled effect presets...")
    rc = subprocess.call(
        [
            "cargo",
            "test",
            "-p",
            "manifold-renderer",
            "--test",
            "bundled_presets_drift",
            "--",
            "--ignored",
        ],
        cwd=REPO_ROOT,
    )
    if rc != 0:
        print("Preset regen failed.")
        return rc

    print("\nRunning workspace lib tests...")
    rc = subprocess.call(["cargo", "test", "--workspace", "--lib"], cwd=REPO_ROOT)
    return rc


if __name__ == "__main__":
    sys.exit(main())
