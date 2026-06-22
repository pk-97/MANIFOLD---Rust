"""Stage 2 — effects + compositing.

Each effect runs on the FULL original image, then composites over the running
result using a chosen object's mask as alpha. Processing the whole frame (not a
cutout) avoids edge halos.

Apply syntax (repeatable, applied in order):
    --apply "TARGET:EFFECT[:k=v,k=v,...]"

TARGET is an object index, label, or slug from objects.json (or "background").
Hard effects default to the tight binary mask; tonal effects to the soft matte.
Override per-apply with mask=binary or mask=soft.

Example:
    python render.py \
      --apply "audio recorder:floyd_steinberg:levels=2" \
      --apply "dried flower:posterize:levels=4"
"""
from __future__ import annotations

import argparse
import json
import os
import sys

import cv2
import numpy as np

import effects


def load_manifest(masks_dir):
    path = os.path.join(masks_dir, "objects.json")
    if not os.path.isfile(path):
        sys.exit(f"no objects.json in {masks_dir}/ — run segment.py first")
    with open(path) as f:
        return json.load(f)


def resolve_target(target, manifest, masks_dir):
    """Return (binary float32 0..1, soft float32 0..1, label)."""
    h, w = manifest["height"], manifest["width"]
    if target.lower() == "background":
        idx = cv2.imread(os.path.join(masks_dir, "index_map.png"), cv2.IMREAD_GRAYSCALE)
        m = (idx == 0).astype(np.float32)
        return m, m, "background"
    # A label/slug hits the merged class region (all flowers); an index hits one
    # instance. So "dried flower" -> whole group, "0" -> a single flower.
    if not target.isdigit():
        for r in manifest.get("regions", []):
            if target == r["slug"] or target.lower() == r["label"].lower():
                binary = cv2.imread(os.path.join(masks_dir, r["binary"]), cv2.IMREAD_GRAYSCALE)
                soft = cv2.imread(os.path.join(masks_dir, r["soft"]), cv2.IMREAD_GRAYSCALE)
                return binary.astype(np.float32) / 255.0, soft.astype(np.float32) / 255.0, r["label"]
    for o in manifest["objects"]:
        if target == str(o["index"]) or target == o["slug"] or target.lower() == o["label"].lower():
            binary = cv2.imread(os.path.join(masks_dir, o["binary"]), cv2.IMREAD_GRAYSCALE)
            soft = cv2.imread(os.path.join(masks_dir, o["soft"]), cv2.IMREAD_GRAYSCALE)
            return binary.astype(np.float32) / 255.0, soft.astype(np.float32) / 255.0, o["label"]
    avail = ", ".join(f"{o['index']}:{o['slug']}" for o in manifest["objects"])
    sys.exit(f"target '{target}' not found. available: {avail}, background")


def parse_apply(spec):
    """'target:effect:k=v,k=v' -> (target, effect, params dict)."""
    parts = spec.split(":")
    if len(parts) < 2:
        sys.exit(f"bad --apply '{spec}', need TARGET:EFFECT[:params]")
    target, eff = parts[0], parts[1]
    params = {}
    if len(parts) >= 3 and parts[2]:
        for kv in parts[2].split(","):
            k, _, v = kv.partition("=")
            params[k.strip()] = v.strip()
    return target, eff, params


def main():
    ap = argparse.ArgumentParser(description="Stage 2: effects + masked compositing")
    ap.add_argument("--masks", default="masks")
    ap.add_argument("--apply", action="append",
                    help="TARGET:EFFECT[:k=v,...]  (repeatable, stacked in order)")
    ap.add_argument("--out", default="out")
    ap.add_argument("--name", default="composite.png")
    ap.add_argument("--list-effects", action="store_true")
    args = ap.parse_args()

    if args.list_effects:
        print("effects:", ", ".join(effects.list_effects()))
        print("hard (default binary mask):", ", ".join(sorted(effects.HARD_EFFECTS)))
        return

    if not args.apply:
        sys.exit("nothing to do — pass at least one --apply (or --list-effects)")

    manifest = load_manifest(args.masks)
    src_bgr = cv2.imread(manifest["image"])
    if src_bgr is None:
        sys.exit(f"cannot read original image {manifest['image']}")
    original = cv2.cvtColor(src_bgr, cv2.COLOR_BGR2RGB)
    result = original.copy()
    os.makedirs(args.out, exist_ok=True)

    for spec in args.apply:
        target, eff, params = parse_apply(spec)
        if eff not in effects.REGISTRY:
            sys.exit(f"unknown effect '{eff}'. options: {', '.join(effects.list_effects())}")
        binary, soft, label = resolve_target(target, manifest, args.masks)

        mask_kind = params.pop("mask", None)
        if mask_kind is None:
            mask_kind = "binary" if eff in effects.HARD_EFFECTS else "soft"
        alpha = (binary if mask_kind == "binary" else soft)[..., None]

        processed = effects.REGISTRY[eff](original, **params)
        result = (processed.astype(np.float32) * alpha +
                  result.astype(np.float32) * (1.0 - alpha)).astype(np.uint8)
        print(f">> {label}: {eff}({params}) via {mask_kind} mask")

    out_path = os.path.join(args.out, args.name)
    cv2.imwrite(out_path, cv2.cvtColor(result, cv2.COLOR_RGB2BGR))
    print(f">> saved {out_path}")


if __name__ == "__main__":
    main()
