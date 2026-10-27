#!/usr/bin/env python3
"""Water scenes: a few GPU FLIP scenes that stress the water at other scales,
domains and contacts than the shipped Dam Break, as projects to open in
MANIFOLD and to time on the live render path.

  water_scenes.py build OUT_DIR           write one .manifold per scene
  water_scenes.py time OUT_DIR [NAME...]  time each (all by default) with
                                          `manifold frame-time`, one line each

`build` needs target/release/project_tool, `time` target/release/manifold
built with --features perf-soak. Each scene is a shipped preset with a few
domain params changed; the project holds it as one generator layer with a
long clip.
"""
import json
import re
import subprocess
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PRESETS = ROOT / "crates/manifold-nodes/tests/fixtures/presets"
PROJECT_TOOL = ROOT / "target/release/project_tool"
MANIFOLD = ROOT / "target/release/manifold"

OPEN = {"type": "Bool", "value": False}

# name: (preset, domain params set on node.gpu_flip_domain, what it shows)
SCENES = {
    "dam": ("WaterDamBreakGpuFlip", {}, "the shipped Dam Break: 4 m tank, 6.25 cm cells"),
    "dam_fine": (
        "WaterDamBreakGpuFlip",
        {"cell_metres": {"type": "Float", "value": 0.03125}},
        "the same tank at half the cell size: 8x the cells, the detail ceiling",
    ),
    "tank_8m": (
        "WaterDamBreakGpuFlip",
        {"domain_size": {"type": "Float", "value": 8.0}},
        "an 8 m tank, same cells: cost should follow the water, not the box",
    ),
    "open_sides": (
        "WaterDamBreakGpuFlip",
        {f"closed_{face}": OPEN for face in ["neg_x", "pos_x", "neg_z", "pos_z"]},
        "floor only: the water spreads past the tank without walls",
    ),
    "no_bounds": (
        "WaterDamBreakGpuFlip",
        # Fill Height fills the box from its floor, so without one only the
        # dam's Initial Fill starts.
        {"bounds": OPEN, "fill_height": {"type": "Float", "value": 0.0}},
        "no box at all: the water falls for ever, its cost should stay flat",
    ),
    "sea_wall": ("WaterSeaWallGpuFlip", {}, "the shipped Sea Wall: waves on a solid wall"),
}


def scene_graph(preset: str, params: dict) -> dict:
    graph = json.loads((PRESETS / f"{preset}.json").read_text())
    domains = []

    def walk(level):
        for node in level["nodes"]:
            if node["typeId"] == "node.gpu_flip_domain":
                domains.append(node)
            if node.get("group"):
                walk(node["group"])

    walk(graph)
    if len(domains) != 1:
        sys.exit(f"{preset}: expected one GPU FLIP domain, found {len(domains)}")
    domains[0]["params"].update(params)
    # A card bound to a changed param starts at the new value, or it would set
    # the old one back on load.
    metadata = graph.get("presetMetadata", {})
    for binding in metadata.get("bindings", []):
        target = binding.get("target", {})
        value = params.get(target.get("param"))
        if target.get("nodeId") == domains[0].get("nodeId") and value is not None:
            binding["defaultValue"] = value["value"]
            for card in metadata.get("params", []):
                if card["id"] == binding["id"]:
                    card["defaultValue"] = value["value"]
    return graph


def build(out: Path):
    if not PROJECT_TOOL.exists():
        sys.exit("build target/release/project_tool first: cargo build --release -p manifold-app --bin project_tool")
    out.mkdir(parents=True, exist_ok=True)
    clips = out / "clips.json"
    clips.write_text(json.dumps([{"start_beat": 0, "duration_beats": 4096}]))
    for name, (preset, params, what) in SCENES.items():
        path = out / f"{name}.manifold"
        path.unlink(missing_ok=True)
        for args in (
            ["project", "new", str(path)],
            ["layer", "add", str(path), "--name", "Water", "--type", "generator"],
            ["clip", "add-bulk", str(path), "--layer", "Water", "--json", str(clips)],
            ["settings", "set-sim-rate", str(path), "60"],
        ):
            subprocess.run([str(PROJECT_TOOL), *args], check=True, capture_output=True)
        # project_tool makes no preset layers: set the layer's graph in place.
        with zipfile.ZipFile(path) as z:
            entries = {info.filename: z.read(info.filename) for info in z.infolist()}
        project = json.loads(entries["project.json"])
        project["projectName"] = name
        project["timeline"]["layers"][0]["genParams"] = {
            "generatorType": preset,
            "params": {},
            "graph": scene_graph(preset, params),
        }
        entries["project.json"] = json.dumps(project).encode()
        with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
            for filename, data in entries.items():
                z.writestr(filename, data)
        print(f"{name:11} {what}")
    clips.unlink()


def number(pattern: str, text: str) -> str:
    found = re.search(pattern, text)
    return found.group(1) if found else "-"


def time(out: Path, names: list[str]):
    if not MANIFOLD.exists():
        sys.exit("build the app first: cargo build --release -p manifold-app --features perf-soak")
    print(f"{'scene':11} {'frame p50':>9} {'p95':>6} {'step':>6} {'render':>6} {'GPU MB':>6}  (GPU ms, 240 frames)")
    for name in names or SCENES:
        path = out / f"{name}.manifold"
        run = subprocess.run(
            [str(ROOT / "scripts/gpu_queue.py"), "--label", "water", "--",
             str(MANIFOLD), "frame-time", str(path), "--frames", "240",
             "--stamp-every", "8", "--stamp-granularity", "node"],
            capture_output=True, text=True,
        )
        text = run.stdout + run.stderr
        (out / f"{name}.log").write_text(text)
        whole = text.split("== whole run ==")[-1]
        cells = [
            number(r"plain frames .*?sum p50 ([0-9.]+)", whole),
            number(r"plain frames .*?sum p50 [0-9.]+ p95 ([0-9.]+)", whole),
            number(r"node\.gpu_flip_step\s+p50\s+([0-9.]+)", whole),
            number(r"node\.render_scene\s+p50\s+([0-9.]+)", whole),
            number(r"GPU memory allocated after the run ([0-9]+) MB", text),
        ]
        print(
            f"{name:11} {cells[0]:>9} {cells[1]:>6} {cells[2]:>6} {cells[3]:>6} {cells[4]:>6}"
            + ("" if run.returncode == 0 else f"  (exit {run.returncode}, see {name}.log)")
        )


def main():
    if len(sys.argv) < 3 or sys.argv[1] not in ("build", "time"):
        sys.exit(__doc__)
    out = Path(sys.argv[2])
    if sys.argv[1] == "build":
        build(out)
    else:
        time(out, sys.argv[3:])


if __name__ == "__main__":
    main()
