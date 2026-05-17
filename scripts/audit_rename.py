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
