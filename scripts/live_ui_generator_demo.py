#!/usr/bin/env python3
"""Create and verify a 32-bar Caustics clip in an EMPTY live test project.

No project/model writes. All geometry is queried from the current window.
Requires default 120 px/beat zoom, one empty Layer 1, and 4/4 time signature.
"""
import argparse
import json
import time

from live_ui import request, run


def demonstrate(socket):
    count = 0

    def send(payload):
        nonlocal count
        count += 1
        return request(socket, payload)

    def observe(contains=""):
        return send({"op": "observe", "contains": contains})["data"]

    def act(action):
        return send({"op": "act", "action": action})

    def click(text=None, name=None, gesture=None):
        return act({"Pointer": {"target": {"Query": {"text": text} if text else {"name": name}},
                                "gesture": gesture or {"Click": {"modifiers": {}}}}})

    def key(key, **modifiers):
        return act({"Key": {"key": key, "modifiers": modifiers}})

    def expect(values, contains=""):
        return run(socket, [{"request": {"op": "observe", "contains": contains},
                             "expect": values, "timeout": 5}])

    def trim(beat):
        current = observe("px/beat")
        rect = current["clips"][0]["rect"]
        point = send({"op": "timeline_point", "beat": beat, "layer": 0})["data"]["point"]
        point["y"] = min(rect["y"] + rect["height"] / 2,
                         current["tracks"]["y"] + current["tracks"]["height"] / 2)
        start = {"x": rect["x"] + rect["width"] - 0.5, "y": point["y"]}
        act({"Pointer": {"target": {"Point": start},
                         "gesture": {"Drag": {"to": {"Point": point}, "steps": 12}}}})
        expect({"data.state.layers.0.clips.0.durationBeats": beat})

    def parameter(name, value):
        target = "param_row." + name + ".value"
        click(name=target, gesture="DoubleClick")
        key("A", command=True)
        act({"Text": {"text": value}})
        key("Enter")
        expect({"data.nodes.0.text": value}, target)

    initial = observe("px/beat")
    layers = initial["state"]["layers"]
    if len(layers) != 1 or layers[0]["clips"] or layers[0]["name"] != "Layer 1":
        raise RuntimeError("demo requires a fresh empty test project; refusing to modify existing work")
    if initial["state"]["timeSignature"] != [4, 4] or [n["text"] for n in initial["nodes"]] != ["120 px/beat"]:
        raise RuntimeError("demo requires 4/4 and default 120 px/beat zoom")
    start_time = time.monotonic()
    click(text="Layer 1", gesture="RightClick")
    click(text="Insert Generator Layer")
    expect({"data.state.layers.1.generator": "Plasma"})
    click(text="Layer 1")
    key("Backspace")
    expect({"data.state.layers.0.name": "Gen 2"})
    point = send({"op": "timeline_point", "beat": 0, "layer": 0})["data"]["point"]
    point["x"] += 1  # Stay inside the half-open track boundary.
    act({"Pointer": {"target": {"Point": point}, "gesture": "DoubleClick"}})
    expect({"data.state.layers.0.clips.0.startBeat": 0})
    # Give the half-beat default clip a usable handle before zooming out.
    trim(4)
    for _ in range(5):
        click(text="−")
    expect({"data.nodes.0.text": "5 px/beat"}, "px/beat")
    trim(128)
    # Verify undo/redo of the actual edge drag, then restore the final duration.
    key("Z", command=True)
    expect({"data.state.layers.0.clips.0.durationBeats": 4})
    key("Z", command=True, shift=True)
    expect({"data.state.layers.0.clips.0.durationBeats": 128})
    click(text="Change")
    click(text="Caustics", gesture="DoubleClick")
    expect({"data.state.layers.0.generator": "Caustics"})
    parameter("speed", "0.35")
    key("Z", command=True)
    expect({"data.nodes.0.text": "0.80"}, "param_row.speed.value")
    key("Z", command=True, shift=True)
    expect({"data.nodes.0.text": "0.35"}, "param_row.speed.value")
    parameter("scale", "4.50")
    parameter("shine", "0.60")
    click(name="transport.play")
    expect({"data.state.playing": True})
    before = observe()["state"]["beat"]
    act({"Step": {"frames": 20}})
    after = observe()["state"]["beat"]
    if after <= before:
        raise RuntimeError("playback did not advance")
    click(name="transport.stop")
    expect({"data.state.playing": False, "data.state.beat": 0})
    final = observe(".value")
    assert len(final["state"]["layers"]) == 1
    assert len(final["state"]["layers"][0]["clips"]) == 1
    return {"ok": True, "seconds": round(time.monotonic() - start_time, 2),
            "direct_requests": count, "state": final["state"],
            "parameters": {n["name"]: n["text"] for n in final["nodes"]}}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True)
    args = parser.parse_args()
    print(json.dumps(demonstrate(args.socket), separators=(",", ":")))
