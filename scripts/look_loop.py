#!/usr/bin/env python3
"""Warm look loop: edit a scene shader or a params file, get the still back in seconds.

    scripts/look_loop.py <project.manifold> --frame F --png OUT.png
        [--params P.json] [--watch DIR]... [--settle K] [--bin PATH]

Starts `manifold look-loop` once: it loads the project, plays to frame F,
pauses and writes OUT.png. Then this polls the watched .wgsl files and the
params file; on a change the warm process re-renders the same paused frame
from the edited text, with no cargo build and no re-simulation. The params
file is a flat JSON object of generator card param ids to numbers (look
params: a simulation param changed here does not re-simulate). A shader
that fails naga's check keeps the last good PNG and prints the error.

The GPU lock (`gpu_queue.hold`) is held for start-up and each render only,
never while waiting. The binary is the stills build with headless tools:
`cargo build --profile stills -p manifold-app --bin manifold --features perf-soak`.
Ctrl-C quits.
"""

import argparse
import subprocess
import sys
import time
from pathlib import Path

import gpu_queue

ROOT = Path(__file__).resolve().parent.parent
SCENE_SHADERS = ROOT / "crates/manifold-nodes-scene/src/node_graph/primitives/shaders"
POLL_SECONDS = 0.2


def stamps(paths):
    """Modification time of every watched file, by path."""
    out = {}
    for path in paths:
        files = sorted(path.glob("*.wgsl")) if path.is_dir() else [path]
        for f in files:
            try:
                out[f] = f.stat().st_mtime_ns
            except FileNotFoundError:
                out[f] = None
    return out


def reply(proc):
    """Echo the process's output up to its one-line reply; return the reply."""
    for line in proc.stdout:
        line = line.rstrip("\n")
        print(line, flush=True)
        if line.startswith(("look-loop: ready", "look-loop: done", "look-loop: error")):
            return line
    sys.exit(f"look-loop: the process exited ({proc.wait()})")


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("project")
    parser.add_argument("--frame", type=int, required=True)
    parser.add_argument("--png", required=True)
    parser.add_argument("--params")
    parser.add_argument("--watch", action="append", default=None,
                        help=f"shader directory (default {SCENE_SHADERS.relative_to(ROOT)})")
    parser.add_argument("--settle", type=int)
    parser.add_argument("--bin", default=str(ROOT / "target/stills/manifold"))
    args = parser.parse_args()

    watch = [Path(w).resolve() for w in (args.watch or [SCENE_SHADERS])]
    command = [args.bin, "look-loop", args.project, "--frame", str(args.frame), "--png", args.png]
    for w in watch:
        command += ["--watch", str(w)]
    if args.params:
        command += ["--params", args.params]
    if args.settle:
        command += ["--settle", str(args.settle)]
    watched = watch + ([Path(args.params)] if args.params else [])

    # Taken before start-up, so an edit made while it loads renders next.
    seen = stamps(watched)
    # The process inherits this hold's token, so its own queued device
    # creation does not wait; later renders take the lock again.
    with gpu_queue.hold("look-loop start"):
        proc = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        # Same binary name as the app; later holds must not wait for it.
        gpu_queue.own_headless_pids.add(proc.pid)
        if not reply(proc).startswith("look-loop: ready"):
            proc.kill()
            return 1
    try:
        while True:
            time.sleep(POLL_SECONDS)
            now = stamps(watched)
            if now == seen:
                continue
            # Let an editor finish writing before reading.
            time.sleep(POLL_SECONDS)
            seen = stamps(watched)
            with gpu_queue.hold("look-loop render"):
                proc.stdin.write("render\n")
                proc.stdin.flush()
                reply(proc)
    except KeyboardInterrupt:
        pass
    finally:
        if proc.poll() is None:
            proc.stdin.close()
            proc.wait()
    return 0


if __name__ == "__main__":
    sys.exit(main())
