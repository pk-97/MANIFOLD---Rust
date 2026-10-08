#!/usr/bin/env python3
"""The repo's tool inventory and front door: `scripts/dev.py --help` lists every
verb in one line; `scripts/dev.py <verb> [args]` runs the tool behind it.

Agents run `--help`, they do not browse ninety files, so a tool that is not
listed here gets rebuilt. Every `scripts/*.py|*.sh` is either a verb below or
named in INTERNAL (a module other tools import, or a retired probe kept for
its numbers); scripts/test_dev.py enforces that, so a new script is listed or
the suite is red. A verb's own `--help` is the tool's usage. Cargo verbs run
from the repo root this file lives in (a worktree runs its own tree).
"""
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"

# (group, verb, target, one line). A str target is a script under scripts/;
# a list is a command run from ROOT, with the verb's arguments appended.
# GPU targets go through gpu_queue so they wait their turn like every lane.
GPU = ["scripts/gpu_queue.py", "--"]
VERBS = [
    ("Landing and gates",
     "gate", "landing_gate.py",
     "landing gate for a branch worktree: every check, one `rerun:` command per red (--repo <worktree>)"),
    ("Landing and gates",
     "land", "land_branch.py",
     "the whole landing ceremony: fetch, merge main in, gate, no-ff merge, push, bead close"),
    ("Landing and gates",
     "land-wave", "land_wave.py", "land one or a batch of wave branches without the main checkout"),
    ("Landing and gates",
     "gpu-proofs", "gpu_proofs_gate.py",
     "GPU proof tests scoped to the branch diff; --filter <test> runs one; --all is nightly"),
    ("Landing and gates",
     "gpu-scope", "gpu_scope.py", "dry run: which GPU proofs these paths select (args: paths)"),
    ("Landing and gates",
     "flows", "run_ui_flows.py",
     "UI flow suite; <name> runs one flow, --touched <range> is the gate's selection"),
    ("Landing and gates",
     "flows-batch-proof", "ui_flows_batch_proof.py", "proof that batched flows report what solo runs report"),
    ("Landing and gates",
     "trunk-health", "trunk_health.py", "nightly workspace sweep on main; files beads on red"),
    ("Landing and gates",
     "landing-metrics", "landing_metrics.py",
     "the landing loop measured over N days: runs per landing, red share, GPU reuse, queue wait"),
    ("Landing and gates",
     "feature-matrix", "feature_matrix.py", "build every non-default feature so none rots"),
    ("Landing and gates",
     "rt-noise", "rt_noise_gate.py", "RT temporal-stability gate on a paused scene (--record to re-baseline)"),
    ("Landing and gates",
     "bridge-probe", "bridge_probe_gate.py", "presentation tear regression gate"),
    ("Landing and gates",
     "crate-move", "crate_move_replay.py", "replay a reviewed crate move or verify full tree identity"),
    ("Landing and gates",
     "move-check", "move_identity_check.py", "prove a pure code move; --plan derives replay maps and reports template digests; --rewrite/--rewrites-file for explicit maps"),
    ("Landing and gates",
     "test-census", "test_census.py", "record and compare test identities across crate moves"),
    ("Landing and gates",
     "crate-closure", "crate_closure.py",
     "renderer crate-split census: `closure` sizes the engine hub, `seams` lists hub->family reaches"),
    ("Landing and gates",
     "docs-index", "gen_docs_index.py", "regenerate docs/README.md after adding or renaming a doc"),
    ("Landing and gates",
     "glb-status", "gen_glb_conformance_status.py", "regenerate the glTF conformance status doc"),
    ("Landing and gates",
     "gate-runner", "gate_runner.py", "machine-written verdict trail for lane gates"),

    ("Machine: GPU, worktrees, disk",
     "gpu-queue", "gpu_queue.py",
     "run any command under the machine-wide GPU lock (`-- <cmd>`); `status` shows who holds it"),
    ("Machine: GPU, worktrees, disk",
     "worktree", "agent-worktree.py", "the slot ring: acquire <task> <branch>, release, retire, list, reclaim"),
    ("Machine: GPU, worktrees, disk",
     "storage", "storage_budget.py", "Cargo target inventory and admission; what reclaim would free"),
    ("Machine: GPU, worktrees, disk",
     "codex-brokers", "codex_brokers.py", "stop idle Codex brokers pinning worktree slots"),
    ("Machine: GPU, worktrees, disk",
     "claude-pane", "claude-pane.sh", "launch a Claude Code session in a new tmux pane without stealing focus"),

    ("Render and measure",
     "capture", GPU + ["cargo", "run", "-p", "manifold-renderer", "--features", "gpu-proofs", "--example", "fluid_capture", "--"],
     "render any preset to PNG frames: OUT_DIR --preset <json> [--frames N] (the visual oracle)"),
    ("Render and measure",
     "render-generator", ["cargo", "run", "-p", "manifold-renderer", "--bin", "render-generator-preset", "--"],
     "render one generator preset headless"),
    ("Render and measure",
     "render-import", ["cargo", "run", "-p", "manifold-app", "--bin", "render-import", "--"],
     "render an imported glTF/GLB headless; --dump-def writes the importer's def JSON"),
    ("Render and measure",
     "graph-tool", ["cargo", "run", "-p", "manifold-app", "--bin", "graph-tool", "--"],
     "validate <file.json> --kind effect|generator, fusion report; pre-flight for graph JSON"),
    ("Render and measure",
     "check-presets", ["cargo", "run", "-p", "manifold-renderer", "--bin", "check-presets", "--"],
     "validate every bundled preset"),
    ("Render and measure",
     "node-catalog", ["cargo", "run", "-p", "manifold-renderer", "--bin", "gen_node_catalog", "--"],
     "regenerate docs/NODE_CATALOG.md from the primitive registry"),
    ("Render and measure",
     "thumbnails", GPU + ["cargo", "run", "-p", "manifold-app", "--bin", "generate-preset-thumbnails", "--"],
     "regenerate preset picker thumbnails"),
    ("Render and measure",
     "freeze-profile", GPU + ["cargo", "run", "-p", "manifold-renderer", "--bin", "freeze-profile", "--"],
     "profile the freeze compiler's fused kernels"),
    ("Render and measure",
     "project-tool", ["cargo", "run", "-p", "manifold-io", "--bin", "project_tool", "--"],
     ".manifold files: info, json, tempo show|set|at, clip add-audio, scene set-model (never hand-edit the ZIP)"),
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
     "depth-relight", "depth_relight_sweep.py", "append the 2.5D relight tail to a preset JSON"),
    ("Render and measure",
     "gltf-def-capture", "gltf_def_capture.py", "capture importer def-JSON dumps for the equivalence gate"),
    ("Render and measure",
     "blob-bench", "blob_v2_native_bench.py", "measure the BlobDetector V2 C ABI"),
    ("Render and measure",
     "kick-labels", "kick_label_extract.py", "ground-truth kick times from drum stems"),

    ("CPU reference oracles (f64, seconds, no GPU)",
     "mgpcg-ref", "mgpcg_reference.py", "GPU FLIP pressure solve oracle"),
    ("CPU reference oracles (f64, seconds, no GPU)",
     "lentine-ref", "lentine_reference.py", "Lentine block projection oracle"),
    ("CPU reference oracles (f64, seconds, no GPU)",
     "narrow-band-ref", "narrow_band_reference.py", "narrow-band particle lifecycle oracle"),
    ("CPU reference oracles (f64, seconds, no GPU)",
     "narrow-band-grid-ref", "narrow_band_grid_reference.py", "narrow-band grid pass oracle"),

    ("Live UI (opt-in connection to a running app)",
     "live-ui-launch", "launch_live_ui.py", "launch the separate macOS test app"),
    ("Live UI (opt-in connection to a running app)",
     "live-ui", "live_ui.py", "JSON client for the live UI connection"),
    ("Live UI (opt-in connection to a running app)",
     "live-ui-generator-demo", "live_ui_generator_demo.py", "create and verify a Caustics clip live"),
    ("Live UI (opt-in connection to a running app)",
     "live-ui-safety-demo", "live_ui_safety_demo.py", "rollback when a live client disconnects mid-gesture"),

    ("Process, beads, usage",
     "fleet-health", "fleet_health.py", "stalls and blockers across lanes (the lead's cron)"),
    ("Process, beads, usage",
     "stale-beads", "stale_beads.py", "open beads untouched past threshold"),
    ("Process, beads, usage",
     "hook-census", "hook_census.py", "which hooks fire, how often, from telemetry"),
    ("Process, beads, usage",
     "token-report", "token_report.py", "Claude Code token spend from local transcripts"),
    ("Process, beads, usage",
     "claude-usage", "claude_usage_export.py", "export Anthropic usage to the fleet dashboard"),
    ("Process, beads, usage",
     "codex-usage", "codex_usage.py", "token usage from Codex session logs"),
    ("Process, beads, usage",
     "codex-prepare", "codex_prepare.py", "a compact source-grounded brief for a worker"),
    ("Process, beads, usage",
     "codex-regressions", "codex_regressions.py", "named regression evidence and the focused command per bead"),
    ("Process, beads, usage",
     "fix-bare-ids", "fix_bare_ids.py", "attach names to bare bead and section IDs in prose"),
    ("Process, beads, usage",
     "fetch-gltf", "fetch-gltf-conformance.sh", "fetch the Khronos glTF sample set"),
    ("Process, beads, usage",
     "ableton-patch", "install-abletonosc-patch.sh", "install the AbletonOSC patch (uninstall-abletonosc-patch.sh reverts)"),
]

# Importable modules, self-tests and retired probes: not verbs, still accounted for.
INTERNAL = {
    "gate_readiness.py", "gate_workspace.py", "gate_policy.py", "dev.py", "cpu_scope.py", "diff_scope.py", "gate_passes.py", "codex_checks.py",
    "audit_rename.py", "rt_a2_term_cost.py", "rt_a3_term_cost.py",
    "gate_runner_selftest.sh", "uninstall-abletonosc-patch.sh",
}


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
            return [str(ROOT / target[0]) if target[0].startswith("scripts/") else target[0],
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
