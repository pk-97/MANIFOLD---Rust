#!/usr/bin/env python3
"""Launch a separate, identifiable macOS test app without touching other instances.

Build first: cargo build -p manifold-app --features ui-automation.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    binary = (args.binary or root / "target/debug/manifold").resolve(strict=True)
    directory = Path(tempfile.mkdtemp(prefix="manifold-ui-", dir="/private/tmp"))
    bundle = directory / "MANIFOLD Live UI.app"
    executable = bundle / "Contents/MacOS/manifold"
    executable.parent.mkdir(parents=True)
    shutil.copy2(binary, executable)
    with (bundle / "Contents/Info.plist").open("wb") as out:
        plistlib.dump({"CFBundleExecutable": "manifold", "CFBundleName": "MANIFOLD Live UI",
                      "CFBundleDisplayName": "MANIFOLD Live UI",
                      "CFBundleIdentifier": "com.manifold.live-ui." + directory.name,
                      "CFBundlePackageType": "APPL", "NSHighResolutionCapable": True}, out)
    env = os.environ.copy()
    env["MANIFOLD_UI_SOCKET"] = str(directory / "ui.sock")
    with (directory / "app.log").open("w") as log:
        process = subprocess.Popen([str(executable)], cwd=root, env=env, stdout=log,
                                   stderr=subprocess.STDOUT, start_new_session=True)
    deadline = time.monotonic() + 45
    while not (directory / "ui.sock").exists():
        if process.poll() is not None:
            raise SystemExit(f"test app exited ({process.returncode}); see {directory / 'app.log'}")
        if time.monotonic() >= deadline:
            raise SystemExit(f"test app did not expose its socket within 45s; PID {process.pid}; see {directory / 'app.log'}")
        time.sleep(0.1)
    print(json.dumps({"pid": process.pid, "bundle": str(bundle),
                      "socket": env["MANIFOLD_UI_SOCKET"], "log": str(directory / "app.log")}))


if __name__ == "__main__":
    main()
