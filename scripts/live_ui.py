#!/usr/bin/env python3
"""Compact JSON client for the opt-in live MANIFOLD UI connection.

Every action uses the running window's input path. Acknowledgement means
dispatched, not that a postcondition passed. Use run/expect for state checks.
"""
import argparse
import json
import socket
import sys
import time


def request(path, payload, timeout=8):
    message = json.dumps(payload, allow_nan=False).encode() + b"\n"
    if len(message) > 65536:
        raise ValueError("request exceeds 64 KiB")
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(timeout)
        connection.connect(path)
        connection.sendall(message)
        data = bytearray()
        while b"\n" not in data:
            block = connection.recv(65536)
            if not block:
                raise RuntimeError("app disconnected before replying; action outcome is unknown")
            data.extend(block)
            if len(data) > 2 * 1024 * 1024:
                raise RuntimeError("response exceeds 2 MiB")
    response = json.loads(data.split(b"\n", 1)[0])
    if not response.get("ok"):
        raise RuntimeError(response.get("error", "request failed"))
    return response


def lookup(value, path):
    for part in path.split("."):
        value = value[int(part)] if isinstance(value, list) else value[part]
    return value


def run(path, steps):
    """Sequential flow; mutation requests are never automatically retried."""
    results = []
    for index, step in enumerate(steps):
        if "expect" in step:
            observation = step.get("request", {"op": "observe", "contains": ""})
            if observation.get("op") not in {"observe", "resolve", "timeline_point"}:
                raise ValueError("expect may poll only a read-only request; edits are never retried")
            deadline = time.monotonic() + min(float(step.get("timeout", 3)), 30)
            while True:
                result = request(path, observation)
                try:
                    passed = all(lookup(result, key) == value for key, value in step["expect"].items())
                except (KeyError, IndexError, TypeError, ValueError):
                    passed = False
                if passed:
                    break
                if time.monotonic() >= deadline:
                    raise RuntimeError(f"step {index} expectation failed: {step['expect']}; observed: {result}")
                time.sleep(0.05)
        else:
            result = request(path, step["request"])
        results.append({"step": index, "frame": result.get("frame"), "ok": True})
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True, help="socket belonging to the intended app instance")
    sub = parser.add_subparsers(dest="command", required=True)
    observe = sub.add_parser("observe")
    observe.add_argument("--contains")
    raw = sub.add_parser("request", help="send one protocol JSON object")
    raw.add_argument("json")
    flow = sub.add_parser("run", help="run requests and bounded state expectations from a JSON file")
    flow.add_argument("file")
    click = sub.add_parser("click")
    target = click.add_mutually_exclusive_group(required=True)
    target.add_argument("--name")
    target.add_argument("--text")
    target.add_argument("--widget", help="hex identity from observe")
    click.add_argument("--under-text")
    kind = click.add_mutually_exclusive_group()
    kind.add_argument("--right", action="store_true")
    kind.add_argument("--double", action="store_true")
    key = sub.add_parser("key")
    key.add_argument("key", help="UI key enum, e.g. Enter, Backspace, Z, Space")
    for modifier in ("command", "ctrl", "alt", "shift"):
        key.add_argument("--" + modifier, action="store_true", dest="mod_" + modifier)
    text = sub.add_parser("type")
    text.add_argument("text")
    point = sub.add_parser("timeline-point")
    point.add_argument("beat", type=float)
    point.add_argument("--layer", type=int, default=0)
    args = parser.parse_args()
    if args.command == "run":
        with open(args.file, encoding="utf-8") as source:
            result = run(args.socket, json.load(source))
    else:
        if args.command == "observe":
            payload = {"op": "observe", "contains": args.contains}
        elif args.command == "request":
            payload = json.loads(args.json)
        elif args.command == "timeline-point":
            payload = {"op": "timeline_point", "beat": args.beat, "layer": args.layer}
        else:
            if args.command == "click":
                if args.widget:
                    target = {"Widget": int(args.widget, 16)}
                else:
                    query = {key: value for key, value in
                             (("name", args.name), ("text", args.text), ("under_text", args.under_text)) if value is not None}
                    target = {"Query": query}
                gesture = "RightClick" if args.right else "DoubleClick" if args.double else {"Click": {"modifiers": {}}}
                action = {"Pointer": {"target": target, "gesture": gesture}}
            elif args.command == "key":
                modifiers = {name: getattr(args, "mod_" + name) for name in ("command", "ctrl", "alt", "shift")}
                action = {"Key": {"key": args.key, "modifiers": modifiers}}
            else:
                action = {"Text": {"text": args.text}}
            payload = {"op": "act", "action": action}
        result = request(args.socket, payload)
    print(json.dumps(result, separators=(",", ":"), allow_nan=False))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError) as error:
        print(f"live-ui: {error}", file=sys.stderr)
        sys.exit(1)
