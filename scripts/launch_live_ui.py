#!/usr/bin/env python3
"""Launch a separate macOS test app, reusing its identity across worktree rebuilds.

Build first: cargo build -p manifold-app --features ui-automation.
"""
import argparse
import fcntl
import json
import os
from pathlib import Path
import plistlib
import shutil
import socket
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--reuse-directory", type=Path,
                        help="Adopt an existing launcher directory and its approved app identity")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    binary = (args.binary or root / "target/debug/manifold").resolve(strict=True)
    state_file = root / "target/live-ui-session.json"
    state_file.parent.mkdir(parents=True, exist_ok=True)
    # The child inherits this descriptor, keeping the lease until it exits.
    # A second launcher must not overwrite the executable of a running app.
    lease = (state_file.parent / "live-ui-session.lock").open("a+")
    try:
        fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        raise SystemExit(f"test app is already running; reuse it or quit it before rebuilding; see {state_file}")
    saved = json.loads(state_file.read_text()) if state_file.exists() else None
    if args.reuse_directory:
        directory = args.reuse_directory.resolve(strict=True)
    elif saved:
        directory = Path(saved["directory"])
    else:
        directory = Path(tempfile.mkdtemp(prefix="manifold-ui-", dir="/private/tmp"))
    bundle = directory / "MANIFOLD Live UI.app"
    socket_path = directory / "ui.sock"
    # Also protect an adopted pre-lease instance or a directory used by
    # another checkout. A stale socket refuses connections; a live one does not.
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
        probe.settimeout(1.0)
        try:
            probe.connect(str(socket_path))
        except (FileNotFoundError, ConnectionRefusedError):
            pass
        else:
            raise SystemExit(f"test app is already running at {socket_path}; quit it before rebuilding")
    executable = bundle / "Contents/MacOS/manifold"
    plist_path = bundle / "Contents/Info.plist"
    identity = "com.manifold.live-ui." + directory.name
    if plist_path.exists():
        with plist_path.open("rb") as source:
            identity = plistlib.load(source)["CFBundleIdentifier"]
        if not identity.startswith("com.manifold.live-ui."):
            raise SystemExit("refusing to replace a bundle that is not a MANIFOLD test app")
    elif args.reuse_directory:
        raise SystemExit("--reuse-directory must contain an existing MANIFOLD Live UI.app")
    elif saved:
        identity = saved["identity"]
    executable.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, executable)
    with plist_path.open("wb") as out:
        plistlib.dump({"CFBundleExecutable": "manifold", "CFBundleName": "MANIFOLD Live UI",
                      "CFBundleDisplayName": "MANIFOLD Live UI",
                      "CFBundleIdentifier": identity,
                      "CFBundlePackageType": "APPL", "NSHighResolutionCapable": True}, out)
    env = os.environ.copy()
    env["MANIFOLD_UI_SOCKET"] = str(socket_path)
    state = {"directory": str(directory), "identity": identity, "bundle": str(bundle),
             "socket": env["MANIFOLD_UI_SOCKET"], "log": str(directory / "app.log")}
    state_file.write_text(json.dumps(state) + "\n")
    # No earlier leased instance is alive, so a leftover socket is stale.
    Path(env["MANIFOLD_UI_SOCKET"]).unlink(missing_ok=True)
    with (directory / "app.log").open("w") as log:
        process = subprocess.Popen([str(executable)], cwd=root, env=env, stdout=log,
                                   stderr=subprocess.STDOUT, start_new_session=True,
                                   pass_fds=(lease.fileno(),))
    state["pid"] = process.pid
    state_file.write_text(json.dumps(state) + "\n")
    deadline = time.monotonic() + 45
    while not (directory / "ui.sock").exists():
        if process.poll() is not None:
            raise SystemExit(f"test app exited ({process.returncode}); see {directory / 'app.log'}")
        if time.monotonic() >= deadline:
            raise SystemExit(f"test app did not expose its socket within 45s; PID {process.pid}; see {directory / 'app.log'}")
        time.sleep(0.1)
    print(json.dumps(state))


if __name__ == "__main__":
    main()
