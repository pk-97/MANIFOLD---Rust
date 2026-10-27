#!/usr/bin/env python3
"""The repo's tool inventory and front door: `scripts/dev.py --help` lists every
verb in one line; `scripts/dev.py <verb> [args]` runs the tool behind it.

Agents run `--help`, they do not browse ninety files, so a tool that is not
listed here gets rebuilt. Every `scripts/*.py|*.sh` is either a verb below or
named in INTERNAL (a module other tools import, or a retired probe kept for
its numbers); scripts/test_dev.py enforces that, so a new script is listed or
the suite is red. A verb's own `--help` is the tool's usage. Cargo verbs run
from the repo root this file lives in.
"""
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"

# (group, verb, target, one line). A str target is a script under scripts/;
# a list is a command run from ROOT, with the verb's arguments appended.
# GPU targets go through gpu_queue so they wait their turn.
GPU = ["scripts/gpu_queue.py", "--"]
VERBS = [
    ("Checks",
     "flows", "run_ui_flows.py",
     "UI flow suite; bare = flows touched since origin/main, <name> runs one flow, --all runs everything"),
    ("Checks",
     "feature-matrix", "feature_matrix.py", "build every non-default feature so none rots"),
    ("Checks",
     "rt-noise", "rt_noise_gate.py", "RT temporal-stability check on a paused scene (--record to re-baseline)"),
    ("Checks",
     "bridge-probe", "bridge_probe_gate.py", "presentation tear regression check"),
    ("Checks",
     "wgsl-check", "wgsl_check.py",
     "naga-validate the hand-composed water shader stacks in seconds, no cargo or GPU; names pick stacks"),
    ("Checks",
     "glb-status", "gen_glb_conformance_status.py", "regenerate the glTF conformance status doc"),

    ("Machine",
     "gpu-queue", "gpu_queue.py",
     "GPU lock: `-- <cmd>` (waits for the MANIFOLD app to quit, else NOT RUN), `status`"),
    ("Machine",
     "claude-pane", "claude-pane.sh", "launch a Claude Code session in a new tmux pane without stealing focus"),

    ("Render and measure",
     "capture", GPU + ["cargo", "run", "-p", "manifold-nodes", "--features", "gpu-proofs", "--example", "fluid_capture", "--"],
     "render any preset to PNG frames: OUT_DIR --preset <json> [--frames N] (the visual oracle)"),
    ("Render and measure",
     "render-generator", ["cargo", "run", "-p", "manifold-nodes", "--bin", "render-generator-preset", "--"],
     "render one generator preset headless"),
    ("Render and measure",
     "render-import", ["cargo", "run", "-p", "manifold-app", "--bin", "render-import", "--"],
     "render an imported glTF/GLB headless; --dump-def writes the importer's def JSON"),
    ("Render and measure",
     "graph-tool", ["cargo", "run", "-p", "manifold-app", "--bin", "graph-tool", "--"],
     "validate <file.json> --kind effect|generator, fusion report; pre-flight for graph JSON"),
    ("Render and measure",
     "check-presets", ["cargo", "run", "-p", "manifold-nodes", "--bin", "check-presets", "--"],
     "validate every bundled preset"),
    ("Render and measure",
     "node-catalog", ["cargo", "run", "-p", "manifold-nodes", "--bin", "gen_node_catalog"],
     "print the registered node index"),
    ("Render and measure",
     "thumbnails", GPU + ["cargo", "run", "-p", "manifold-app", "--bin", "generate-preset-thumbnails", "--"],
     "regenerate preset picker thumbnails"),
    ("Render and measure",
     "freeze-profile", GPU + ["cargo", "run", "-p", "manifold-nodes", "--bin", "freeze-profile", "--"],
     "profile the freeze compiler's fused kernels"),
    ("Render and measure",
     "project-tool", ["cargo", "run", "-p", "manifold-app", "--bin", "project_tool", "--"],
     ".manifold files: info, json, project new|upgrade, layer add, tempo show|set|at, clip add-audio|add-bulk|list, marker add-bulk|list, scene set-model (never hand-edit the ZIP)"),
    ("Render and measure",
     "water-scenes", "water_scenes.py",
     "GPU FLIP scenes at other scales, domains and contacts: build OUT_DIR writes projects, time OUT_DIR [NAME...] prints frame, step, render and GPU memory"),
    ("Render and measure",
     "rt-toggle-matrix", "rt_toggle_matrix.py", "one rt-capture per RT toggle; the RT A/B harness"),
    ("Render and measure",
     "rt-quality-matrix", "rt_quality_matrix.py", "RT quality oracle against the committed baseline"),
    ("Render and measure",
     "rt-region-probe", "rt_region_probe.py", "region-mean probe for the multi-bounce GI gate"),
    ("Render and measure",
     "rt-acceptance", "rt_dynamic_acceptance.py", "scene-modifier RT acceptance runner"),
    ("Render and measure",
     "clay-probe", "clay_region_probe.py", "region-mean convergence probe for the Solid render mode"),
    ("Render and measure",
     "points-probe", "points_pixel_probe.py", "non-zero-pixel probe for the Points render mode"),
    ("Render and measure",
     "pick-probe-regions", "pick_probe_regions.py", "choose probe rects from RT captures"),
    ("Render and measure",
     "gltf-def-capture", "gltf_def_capture.py", "capture importer def-JSON dumps for the equivalence gate"),
    ("Render and measure",
     "blob-bench", "blob_v2_native_bench.py", "measure the BlobDetector V2 C ABI"),
    ("Render and measure",
     "kick-labels", "kick_label_extract.py", "ground-truth kick times from drum stems"),
    ("Render and measure",
     "detector-release", ["tools/audio_analysis/lab/release.py"],
     "any lab detector's release: train CONFIG [--provisional] [--nets N], export ID --set NAME [--ship] (model file v2 and parity goldens), parts"),
    ("Render and measure",
     "lab-songs", ["tools/audio_analysis/lab/registry.py"],
     "detector lab song list: build | show [SONG] | verify (audio and hashes match the legacy loader)"),
    ("Render and measure",
     "lab-audit", ["tools/audio_analysis/lab/audit.py"],
     "detector lab label audit for a family: offset proofs, kept known times, generated labels vs project notes"),
    ("Render and measure",
     "lab-offsets", ["tools/audio_analysis/lab/offsets.py"],
     "detector lab export-offset proof for project songs (kick and snare grid fit on the song's own mix)"),
    ("Render and measure",
     "lab-test", ["tools/audio_analysis/lab/test_lab.py"], "detector lab invariant tests"),
    ("Render and measure",
     "lab-run", ["tools/audio_analysis/lab/run.py"],
     "detector lab: train an experiment config's missing seeds, one process each, saving held-out predictions (--plan, --warm)"),
    ("Render and measure",
     "lab-score", ["tools/audio_analysis/lab/score.py"],
     "detector lab: score a run from its saves in seconds (exam and generated songs apart); --legacy NPZ for parity"),
    ("Render and measure",
     "lab-compare", ["tools/audio_analysis/lab/compare.py"],
     "detector lab A/B of two experiment configs: trains missing seeds interleaved, refuses unfair pairs, prints the comparison"),
    ("Render and measure",
     "lab-queue", ["tools/audio_analysis/lab/overnight.py"],
     "detector lab overnight queue: PLAN.json of run/compare steps, one at a time under the GPU lock, a log per step and a summary, stops on the first failure (--dry-run)"),
    ("Render and measure",
     "lab-preflight", ["tools/audio_analysis/lab/preflight.py"],
     "detector lab: read-only check that an overnight PLAN.json can run (clean tree, MPS, disk, configs, kick verdicts, caches warm, GPU lock)"),
    ("Render and measure",
     "lab-review", ["tools/audio_analysis/lab/review_project.py"],
     "detector label review project: build OUT.manifold [--songs] [--fires-dir] [--dry-run] | fires DIR | read ORIGINAL EDITED (Peter's edits as per-family overlays)"),
    ("Render and measure",
     "repair-audio-fixture", "repair_audio_fixture.py", "rebuild a fixture mix from aligned stems, preserving the original; dry-run unless --apply"),
    ("Render and measure",
     "sync-rig-report", "sync_rig_report.py",
     "read a sync rig log (sync-*.log): tick jitter, Link/CLK/AbletonOSC phase, present delay, display latency [LOG...|--latest]"),

    ("CPU reference oracles (f64, seconds, no GPU)",
     "mgpcg-ref", "mgpcg_reference.py", "GPU FLIP pressure solve oracle"),
    ("CPU reference oracles (f64, seconds, no GPU)",
     "lentine-ref", "lentine_reference.py", "Lentine block projection oracle"),

    ("Live UI (opt-in connection to a running app)",
     "live-ui-launch", "launch_live_ui.py", "launch the separate macOS test app"),
    ("Live UI (opt-in connection to a running app)",
     "live-ui", "live_ui.py", "JSON client for the live UI connection"),
    ("Live UI (opt-in connection to a running app)",
     "live-ui-generator-demo", "live_ui_generator_demo.py", "create and verify a Caustics clip live"),
    ("Live UI (opt-in connection to a running app)",
     "live-ui-safety-demo", "live_ui_safety_demo.py", "rollback when a live client disconnects mid-gesture"),

    ("Setup",
     "fetch-gltf", "fetch-gltf-conformance.sh", "fetch the Khronos glTF sample set"),
    ("Setup",
     "ableton-patch", "install-abletonosc-patch.sh", "install the AbletonOSC patch (uninstall-abletonosc-patch.sh reverts)"),
]

# Importable modules and retired probes: not verbs, still accounted for.
INTERNAL = {
    "dev.py", "uninstall-abletonosc-patch.sh",
    "narrow_band_reference.py", "narrow_band_grid_reference.py",
}


def repo_script(command):
    return command[0].endswith((".py", ".sh")) and not command[0].startswith("scripts/")


INDEX = SCRIPTS / "TOOLS.md"


def catalog():
    """The inventory, grouped; each verb names the command behind it so the
    tool can also be run directly."""
    width = max(len(verb) for _, verb, _, _ in VERBS)
    lines = ["# Repo tools",
             "",
             "Generated by `scripts/dev.py --write-index`; `scripts/dev.py --help` prints the same.",
             "`scripts/dev.py <verb> [args]` runs the command in brackets; `<verb> --help` is its usage.",
             ""]
    group = None
    for g, verb, target, line in VERBS:
        if g != group:
            group = g
            lines += ["", f"{g}:"]
        behind = f"scripts/{target}" if isinstance(target, str) else " ".join(target)
        lines.append(f"  {verb:<{width}}  {line}  [{behind}]")
    return "\n".join(lines) + "\n"


def command_for(verb, args):
    for _, name, target, _ in VERBS:
        if name == verb:
            if isinstance(target, str):
                return [str(SCRIPTS / target), *args]
            return [str(ROOT / target[0]) if target[0].startswith("scripts/") or repo_script(target) else target[0],
                    *target[1:], *args]
    return None


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if not argv or argv[0] in {"-h", "--help", "help"}:
        print(catalog(), end="")
        return 0
    if argv[0] == "--write-index":
        INDEX.write_text(catalog())
        print(f"wrote {INDEX}")
        return 0
    command = command_for(argv[0], argv[1:])
    if command is None:
        print(f"unknown verb {argv[0]!r}; run scripts/dev.py --help", file=sys.stderr)
        return 2
    os.chdir(ROOT)
    return subprocess.call(command)


if __name__ == "__main__":
    sys.exit(main())
