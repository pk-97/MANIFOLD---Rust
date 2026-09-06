#!/usr/bin/env python3
"""Exercise rollback when a live UI clip trim client disconnects mid-gesture.

The script only accepts the prepared Caustics test state and refuses to mutate
any other project.  The interrupted drag is deliberately sent on a raw socket
so the client closes before reading the acknowledgement.
"""
import argparse
import json
import math
import socket
import time

from live_ui import request, run


def disconnect_drag(path, start, end, steps, delay):
    payload = {"op": "act", "action": {"Pointer": {"target": {"Point": start},
        "gesture": {"Drag": {"to": {"Point": end}, "steps": steps}}}}}
    message = json.dumps(payload, allow_nan=False).encode() + b"\n"
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(2)
        connection.connect(path)
        connection.sendall(message)
        time.sleep(delay)


def demonstrate(path, disconnect_after=0.15):
    if not math.isfinite(disconnect_after) or not 0 < disconnect_after <= 0.3:
        raise ValueError("disconnect delay must be greater than 0 and at most 0.3 seconds")
    initial = request(path, {"op": "observe", "contains": "px/beat"})["data"]
    layers = initial["state"]["layers"]
    clips = layers[0]["clips"] if len(layers) == 1 else []
    if (len(layers) != 1 or len(clips) != 1 or layers[0].get("generator") != "Caustics"
            or clips[0].get("startBeat") != 0 or clips[0].get("durationBeats") != 128
            or initial["state"].get("playing") is not False
            or [node["text"] for node in initial["nodes"]] != ["5 px/beat"]):
        raise RuntimeError("safety demo requires one stopped Caustics clip at start 0, duration 128, and 5 px/beat; refusing to modify existing work")

    visible = [clip for clip in initial["clips"] if clip["id"] == clips[0]["id"]]
    if len(visible) != 1:
        raise RuntimeError("test clip is not uniquely visible; refusing to modify existing work")
    rect = visible[0]["rect"]
    point = request(path, {"op": "timeline_point", "beat": 64, "layer": 0})["data"]["point"]
    point["y"] = min(rect["y"] + rect["height"] / 2,
                      initial["tracks"]["y"] + initial["tracks"]["height"] / 2)
    start = {"x": rect["x"] + rect["width"] - 0.5, "y": point["y"]}
    disconnect_drag(path, start, point, 60, disconnect_after)

    restored = run(path, [{"request": {"op": "observe", "contains": "px/beat"},
                           "expect": {"data.state.layers.0.clips.0.startBeat": 0,
                                      "data.state.layers.0.clips.0.durationBeats": 128,
                                      "data.input.lastInterruption.reason": "disconnect",
                                      "data.input.lastInterruption.buttonHeld": True,
                                      "data.input.mousePressed": False,
                                      "data.input.textSelecting": False,
                                      "data.input.cursor": initial["input"]["cursor"]},
                           "timeout": 5}])
    after = request(path, {"op": "observe", "contains": "px/beat"})
    interruption = after["data"]["input"]["lastInterruption"]
    old = initial["input"].get("lastInterruption")
    if (old and interruption["frame"] <= old["frame"]) or not 0 < interruption["remainingEvents"] < 64:
        raise RuntimeError("disconnect did not interrupt an in-flight trim; no recovery claim can be made")
    trim = {"op": "act", "action": {"Pointer": {"target": {"Point": start},
        "gesture": {"Drag": {"to": {"Point": point}, "steps": 60}}}}}
    checks = run(path, [{"request": trim},
                        {"request": {"op": "observe"}, "expect": {"data.state.layers.0.clips.0.durationBeats": 64}},
                        {"request": {"op": "act", "action": {"Key": {"key": "Z", "modifiers": {"command": True}}}}},
                        {"request": {"op": "observe"}, "expect": {"data.state.layers.0.clips.0.durationBeats": 128}},
                        {"request": {"op": "act", "action": {"Key": {"key": "Z", "modifiers": {"command": True, "shift": True}}}}},
                        {"request": {"op": "observe"}, "expect": {"data.state.layers.0.clips.0.durationBeats": 64}},
                        {"request": {"op": "act", "action": {"Key": {"key": "Z", "modifiers": {"command": True}}}}},
                        {"request": {"op": "observe"}, "expect": {"data.state.layers.0.clips.0.durationBeats": 128}}])
    return {"ok": True, "disconnect_after": disconnect_after, "restored": restored, "interruption": interruption, "checks": checks}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True)
    parser.add_argument("--disconnect-after", type=float, default=0.15)
    args = parser.parse_args()
    print(json.dumps(demonstrate(args.socket, args.disconnect_after), separators=(",", ":")))
