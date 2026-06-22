"""Stage 1 — text-prompted segmentation.

Grounding DINO -> boxes from text prompts. SAM 2 -> one mask per box.
Per-pixel argmax over mask probability (confidence as tiebreak) gives each
pixel a single owner; everything unowned is the "background" (the black table).

Outputs to ./masks/:
  obj{i}_{label}_soft.png    soft matte (feathered, for tonal effects)
  obj{i}_{label}_binary.png  tight binary (for hard effects: dither/threshold)
  index_map.png              single-channel ownership map (0=bg, 1..N objects)
  overlay.png                color-coded QA preview over the original
  objects.json               manifest consumed by render.py

The original image is never modified.
"""
from __future__ import annotations

import argparse
import json
import os
import sys

import cv2
import numpy as np
from PIL import Image

import matte

DINO_ID = "IDEA-Research/grounding-dino-tiny"
SAM2_ID = "facebook/sam2-hiera-large"

# Distinct colors for the QA overlay / index visualization.
PALETTE = np.array([
    [0, 0, 0], [231, 76, 60], [46, 204, 113], [52, 152, 219],
    [241, 196, 15], [155, 89, 182], [26, 188, 156], [230, 126, 34],
    [236, 64, 122], [149, 165, 166], [120, 224, 143], [255, 138, 101],
], dtype=np.uint8)


def pick_device() -> str:
    import torch
    if torch.backends.mps.is_available():
        return "mps"
    if torch.cuda.is_available():
        return "cuda"
    return "cpu"


def _iou(a, b):
    ix0, iy0 = max(a[0], b[0]), max(a[1], b[1])
    ix1, iy1 = min(a[2], b[2]), min(a[3], b[3])
    iw, ih = max(0.0, ix1 - ix0), max(0.0, iy1 - iy0)
    inter = iw * ih
    ua = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter
    return inter / ua if ua > 0 else 0.0


def nms(boxes, labels, scores, iou_thr):
    """Greedy NMS. High threshold: drops only near-duplicate boxes (same object
    matched twice), keeps genuinely separate but touching objects (flowers)."""
    order = sorted(range(len(boxes)), key=lambda i: scores[i], reverse=True)
    keep = []
    while order:
        i = order.pop(0)
        keep.append(i)
        order = [j for j in order if _iou(boxes[i], boxes[j]) < iou_thr]
    return ([boxes[i] for i in keep], [labels[i] for i in keep],
            [scores[i] for i in keep])


def detect(image_pil, prompts, device, box_thr, text_thr, iou_thr=0.8):
    """Grounding DINO, run once PER PROMPT so every box gets a clean, single
    label (no token-mashing). Boxes are then deduped with NMS.
    Returns (boxes_xyxy[N], labels[N], scores[N])."""
    import torch
    from transformers import (AutoModelForZeroShotObjectDetection,
                              AutoProcessor)
    processor = AutoProcessor.from_pretrained(DINO_ID)
    model = AutoModelForZeroShotObjectDetection.from_pretrained(DINO_ID).to(device)
    all_boxes, all_labels, all_scores = [], [], []
    for prompt in prompts:
        text = prompt.strip().lower() + "."   # one prompt per pass
        inputs = processor(images=image_pil, text=text, return_tensors="pt").to(device)
        with torch.no_grad():
            outputs = model(**inputs)
        res = processor.post_process_grounded_object_detection(
            outputs, inputs.input_ids,
            threshold=box_thr, text_threshold=text_thr,
            target_sizes=[image_pil.size[::-1]],
        )[0]
        for box, score in zip(res["boxes"].cpu().numpy(), res["scores"].cpu().numpy()):
            all_boxes.append(box)
            all_labels.append(prompt.strip().lower())   # the clean source prompt
            all_scores.append(float(score))
    if not all_boxes:
        return [], [], []
    return nms(all_boxes, all_labels, all_scores, iou_thr)


def segment_prompts(image_np, items, device):
    """SAM 2 per prompt. Each item carries a 'box' xyxy OR a 'point' (x, y).
    Returns (prob_maps NxHxW float32, iou_scores[N])."""
    from sam2.sam2_image_predictor import SAM2ImagePredictor
    predictor = SAM2ImagePredictor.from_pretrained(SAM2_ID, device=device)
    predictor.set_image(image_np)
    probs, ious = [], []
    for it in items:
        if "box" in it:
            masks, scores, _ = predictor.predict(
                box=it["box"][None, :], multimask_output=True, return_logits=True)
        else:
            x, y = it["point"]
            masks, scores, _ = predictor.predict(
                point_coords=np.array([[x, y]], dtype=np.float32),
                point_labels=np.array([1]),     # 1 = positive (this object)
                multimask_output=True, return_logits=True)
        best = int(np.argmax(scores))
        logit = masks[best].astype(np.float32)
        probs.append(1.0 / (1.0 + np.exp(-logit)))   # sigmoid -> 0..1
        ious.append(float(scores[best]))
    return np.stack(probs, 0) if probs else np.empty((0,) + image_np.shape[:2]), ious


def resolve_ownership(probs, ious, priority=None, fg_thr=0.5):
    """Per-pixel single owner. index_map: 0=bg, 1..N = object i (1-based).
    `priority` weights each instance in the argmax — manual points get a high
    value so the user's explicit mark wins overlaps over a text guess."""
    n, h, w = probs.shape
    if n == 0:
        return np.zeros((h, w), np.int32)
    w_iou = np.array(ious, np.float32)
    w_pri = np.ones(n, np.float32) if priority is None else np.array(priority, np.float32)
    weighted = probs * (w_iou * w_pri)[:, None, None]
    owner = np.argmax(weighted, axis=0)               # 0..N-1
    has_fg = probs.max(axis=0) >= fg_thr              # at least one real claim
    index_map = np.where(has_fg, owner + 1, 0).astype(np.int32)
    return index_map


def clean_index_map(index_map, n):
    """Fill interior holes and drop stray specks per instance, without ever
    letting one object claim a pixel already owned by another (background only).
    Fixes 'patchy' masks: buttons/grilles/petal-centers SAM left unfilled."""
    from scipy import ndimage
    out = index_map.copy()
    for i in range(1, n + 1):
        m = index_map == i
        if not m.any():
            continue
        lbl, k = ndimage.label(m)
        if k > 1:                                   # keep only the largest blob
            counts = np.bincount(lbl.ravel())
            counts[0] = 0
            biggest = int(counts.argmax())
            out[m & (lbl != biggest)] = 0
            m = lbl == biggest
        filled = ndimage.binary_fill_holes(m)       # close interior holes
        add = filled & ~m & (out == 0)              # only reclaim background
        out[add] = i
    return out


def suppress_table_leak(index_map, image_rgb, protect, sat_keep=60):
    """Reclaim table pixels that a colorful object's mask grew over.
    `protect` is a set of 1-based instance ids to leave fully intact (the
    desaturated gadgets — silver recorder, dark watch). Every other instance
    (the flowers) has its near-grey pixels trimmed as table leak."""
    S = cv2.cvtColor(image_rgb, cv2.COLOR_RGB2HSV)[..., 1]
    out = index_map.copy()
    for i in range(1, index_map.max() + 1):
        if i in protect:
            continue
        out[(index_map == i) & (S < sat_keep)] = 0
    return out


def main():
    ap = argparse.ArgumentParser(description="Stage 1: prompt-driven segmentation")
    ap.add_argument("--image", required=True)
    ap.add_argument("--prompts", nargs="+", default=[],
                    help="text prompts, e.g. 'audio recorder' 'dried flower'")
    ap.add_argument("--point", action="append", default=[],
                    help="manual SAM point for hard objects: 'x,y:label' "
                         "(pixel coords in the original image). Repeatable.")
    ap.add_argument("--out", default="masks")
    ap.add_argument("--box-threshold", type=float, default=0.30)
    ap.add_argument("--text-threshold", type=float, default=0.25)
    ap.add_argument("--matte", choices=["guided", "pymatting"], default="guided")
    ap.add_argument("--band", type=int, default=12, help="edge feather width (px)")
    ap.add_argument("--no-clean", action="store_true",
                    help="skip hole-fill / largest-component cleanup")
    ap.add_argument("--no-trim-table", action="store_true",
                    help="skip per-instance grey-table suppression")
    ap.add_argument("--table-sat", type=int, default=60,
                    help="saturation below which a colorful object's pixels are "
                         "treated as table leak (0-255)")
    args = ap.parse_args()

    if not os.path.isfile(args.image):
        sys.exit(f"image not found: {args.image}")
    os.makedirs(args.out, exist_ok=True)

    device = pick_device()
    print(f">> device: {device}")
    os.environ.setdefault("PYTORCH_ENABLE_MPS_FALLBACK", "1")

    image_pil = Image.open(args.image).convert("RGB")
    image_np = np.array(image_pil)            # HxWx3 uint8 RGB, the untouched source
    h, w = image_np.shape[:2]

    if not args.prompts and not args.point:
        sys.exit("give at least one --prompts or --point")

    items = []
    if args.prompts:
        print(">> detecting (Grounding DINO)…")
        boxes, labels, scores = detect(image_pil, args.prompts, device,
                                       args.box_threshold, args.text_threshold)
        for b, l, s in zip(boxes, labels, scores):
            items.append({"box": b, "label": l, "score": s})
    for pstr in args.point:                       # manual points for hard objects
        coord, _, lab = pstr.partition(":")
        try:
            x, y = (float(v) for v in coord.split(","))
        except ValueError:
            sys.exit(f"bad --point '{pstr}', expected 'x,y:label'")
        items.append({"point": (x, y), "label": lab.strip().lower() or "object",
                      "score": 1.0})
    if not items:
        sys.exit("no detections — loosen --box-threshold or add a --point")

    labels = [it["label"] for it in items]
    scores = [it["score"] for it in items]
    print(f">> {len(items)} prompts -> SAM 2 masks…")
    probs, ious = segment_prompts(image_np, items, device)
    priority = [5.0 if "point" in it else 1.0 for it in items]  # manual marks win
    index_map = resolve_ownership(probs, ious, priority)
    if not args.no_clean:
        index_map = clean_index_map(index_map, len(items))
    if not args.no_trim_table:
        # Protect desaturated gadgets (manual points + recorder/watch/phone labels);
        # trim grey table leak from the colorful flower instances.
        gadget_kw = ("recorder", "watch", "phone", "metal", "silver")
        protect = {i + 1 for i, it in enumerate(items)
                   if "point" in it or any(k in it["label"] for k in gadget_kw)}
        index_map = suppress_table_leak(index_map, image_np, protect,
                                        sat_keep=args.table_sat)

    # Build per-instance outputs from the resolved ownership (no double-claimed px).
    manifest = {"image": os.path.abspath(args.image), "width": w, "height": h,
                "objects": []}
    counts = {}
    subject_soft = np.zeros((h, w), np.float32)   # feathered union of all objects
    for i in range(len(items)):
        raw = labels[i] if i < len(labels) and labels[i] else "object"
        slug = "".join(c if c.isalnum() else "_" for c in raw.strip()).strip("_") or "object"
        counts[slug] = counts.get(slug, 0) + 1
        if counts[slug] > 1:
            slug = f"{slug}{counts[slug]}"

        binary = (index_map == (i + 1)).astype(np.uint8) * 255
        if binary.max() == 0:
            print(f"   - skip obj{i} ({raw}): fully occluded after overlap resolve")
            continue
        soft = matte.refine(image_np, binary, backend=args.matte, band=args.band)
        soft_u8 = (soft * 255).astype(np.uint8)
        subject_soft = np.maximum(subject_soft, soft)

        b_path = os.path.join(args.out, f"obj{i}_{slug}_binary.png")
        s_path = os.path.join(args.out, f"obj{i}_{slug}_soft.png")
        cv2.imwrite(b_path, binary)
        cv2.imwrite(s_path, soft_u8)
        manifest["objects"].append({
            "index": i, "label": raw, "slug": slug,
            "score": round(float(scores[i]), 3),
            "iou": round(float(ious[i]), 3),
            "binary": os.path.basename(b_path),
            "soft": os.path.basename(s_path),
            "pixels": int((binary > 0).sum()),
        })

    # Merged class regions: union all instances sharing a label (e.g. every
    # flower -> one clean "dried flower" region). Kept alongside the per-instance
    # masks. Only emitted for labels with >=2 instances.
    manifest["regions"] = []
    by_label = {}
    for o in manifest["objects"]:
        by_label.setdefault(o["label"], []).append(o["index"])
    for label, idxs in by_label.items():
        if len(idxs) < 2:
            continue
        region = np.zeros((h, w), np.uint8)
        for i in idxs:
            region |= (index_map == (i + 1)).astype(np.uint8)
        region_bin = region * 255
        region_soft = matte.refine(image_np, region_bin, backend=args.matte, band=args.band)
        slug = "".join(c if c.isalnum() else "_" for c in label).strip("_")
        rb = os.path.join(args.out, f"region_{slug}_binary.png")
        rs = os.path.join(args.out, f"region_{slug}_soft.png")
        cv2.imwrite(rb, region_bin)
        cv2.imwrite(rs, (region_soft * 255).astype(np.uint8))
        manifest["regions"].append({
            "label": label, "slug": slug, "instances": idxs,
            "binary": os.path.basename(rb), "soft": os.path.basename(rs),
            "pixels": int((region_bin > 0).sum()),
        })

    # Background (the table) as its own mask, plus a ready-to-use cutout.
    bg_binary = (index_map == 0).astype(np.uint8) * 255
    subject_u8 = (subject_soft * 255).astype(np.uint8)        # keep-subjects alpha
    cv2.imwrite(os.path.join(args.out, "background_binary.png"), bg_binary)
    cv2.imwrite(os.path.join(args.out, "subject_soft.png"), subject_u8)
    # RGBA cutout: original with the table knocked out to transparent.
    cutout = cv2.cvtColor(image_np, cv2.COLOR_RGB2BGRA)
    cutout[:, :, 3] = subject_u8
    cv2.imwrite(os.path.join(args.out, "cutout.png"), cutout)

    # Index map (single channel) + color overlay for QA.
    cv2.imwrite(os.path.join(args.out, "index_map.png"), index_map.astype(np.uint8))
    color = PALETTE[np.clip(index_map, 0, len(PALETTE) - 1)]
    overlay = (0.5 * image_np + 0.5 * color).astype(np.uint8)
    overlay[index_map == 0] = image_np[index_map == 0]  # leave background as-is
    cv2.imwrite(os.path.join(args.out, "overlay.png"),
                cv2.cvtColor(overlay, cv2.COLOR_RGB2BGR))

    with open(os.path.join(args.out, "objects.json"), "w") as f:
        json.dump(manifest, f, indent=2)

    # One-line summary.
    bg_px = int((index_map == 0).sum())
    parts = [f"{o['label']}({o['score']})" for o in manifest["objects"]]
    print(f">> SUMMARY: {len(manifest['objects'])} objects: "
          f"{', '.join(parts)} | background {100*bg_px/(h*w):.1f}% -> {args.out}/")


if __name__ == "__main__":
    main()
