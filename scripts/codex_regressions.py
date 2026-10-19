#!/usr/bin/env python3
"""Named regression evidence and focused commands; never runs a build or render."""
import argparse
import json
from pathlib import Path
import re
import shlex


def inventory(repo, paths=None):
    repo = Path(repo).resolve()
    entries = json.loads((repo / "scripts/codex_regressions.json").read_text())
    result = []
    for name, spec in entries.items():
        source = (repo / spec["source"]).resolve()
        if not source.is_relative_to(repo):
            raise ValueError(f"{name}: source escapes repository")
        text = source.read_text()
        if spec["kind"] == "ui-flow":
            flows = json.loads((repo / "scripts/ui-flows/manifest.json").read_text())["flows"]
            if any(test not in flows for test in spec["tests"]):
                raise ValueError(f"{name}: flow is absent or not a required passing flow")
            actions = json.loads(text)
            if not any(isinstance(a, dict) and "Assert" in a for a in actions):
                raise ValueError(f"{name}: flow has no assertions")
            commands = [["python3", str(repo / "scripts/run_ui_flows.py"), *spec["tests"]]]
        else:
            for test in spec["tests"]:
                if not re.search(r"#\[test\]\s*fn\s+" + re.escape(test) + r"\s*\(", text):
                    raise ValueError(f"{name}: missing executable test {test}")
            if spec["kind"] == "gpu":
                command = ["python3", str(repo / "scripts/gpu_proofs_gate.py"), "--manifest-path", str(repo / "Cargo.toml")]
                for test in spec["tests"]:
                    command.extend(["--filter", test])
                commands = [command]
            elif spec["kind"] == "cpu":
                command = ["cargo", "test", "--manifest-path", str(repo / "Cargo.toml"), "-p", spec["package"]]
                command.extend(["--lib"] if spec["target"] == "lib" else ["--test", spec["target"]])
                commands = [command + [test] for test in spec["tests"]]
            else:
                raise ValueError(f"{name}: unsupported kind {spec['kind']}")
        if paths is None or any(p.startswith(prefix) for p in paths for prefix in spec["triggers"]):
            result.append({"name": name, "source": str(source), "tests": spec["tests"],
                           "kind": spec["kind"], "commands": commands, "cwd": str(repo)})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--category")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    try:
        result = inventory(args.repo)
        if args.category:
            result = [item for item in result if item["name"] == args.category]
            if not result:
                raise ValueError(f"unknown category: {args.category}")
    except (OSError, ValueError, KeyError) as exc:
        parser.error(str(exc))
    if args.json:
        print(json.dumps(result, indent=2))
    else:
        print("References verified; runtime results require executing the commands under the build lock.")
        for item in result:
            print(f"{item['name']}: {item['source']}")
            for command in item["commands"]:
                print(shlex.join(command))


if __name__ == "__main__":
    main()
