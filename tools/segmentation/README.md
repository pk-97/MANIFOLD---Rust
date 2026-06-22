# Segmentation + Effects Pipeline

Text-prompted instance segmentation (Grounding DINO + SAM 2) followed by a
modular, maskable effects compositor. macOS / Apple Silicon, MPS backend.

> **SAM 3 note:** SAM 3 has no public weights yet, so this ships on **SAM 2**
> (`facebook/sam2-hiera-large`). The model id is a one-line config change in
> `segment.py` once SAM 3 weights are released.

## Install

```bash
bash tools/segmentation/setup.sh           # venv + deps (~350 MB)
bash tools/segmentation/setup.sh --weights # also pre-pull weights (~1.6 GB)
source tools/segmentation/.venv/bin/activate
```

Weights download lazily on the first `segment.py` run if you skip `--weights`,
with a tqdm progress bar. Total one-time download ~2–2.5 GB. The venv and
weights are gitignored — they never enter the Rust repo.

## Stage 1 — segment

```bash
python segment.py --image /path/to/photo.jpg \
  --prompts "audio recorder" "dried flower"
```

Writes to `./masks/`: per-object `*_soft.png` + `*_binary.png`, `index_map.png`,
`overlay.png` (QA), and `objects.json`. Prints a one-line object summary.
The original image is never modified.

Per-pixel ownership is resolved by argmax over SAM mask probability (instance
IoU as tiebreak), so no pixel belongs to two objects. Everything unowned is the
`background`.

Useful flags: `--box-threshold 0.30`, `--text-threshold 0.25`,
`--matte guided|pymatting`, `--band 12`.

## Stage 2 — effects

```bash
python render.py \
  --apply "audio recorder:floyd_steinberg:levels=2" \
  --apply "dried flower:posterize:levels=4"
# background stays the untouched black table
```

Each effect runs on the full image, then composites over the running result via
the target's mask. Stack as many `--apply` as you like; they layer in order.

- `--apply "TARGET:EFFECT[:k=v,k=v]"` — TARGET = index, label, slug, or `background`
- Hard effects default to the binary mask; tonal to the soft matte. Override
  with `mask=binary` / `mask=soft` in the params.
- `python render.py --list-effects` to see them all.

### Effects

| name | type | params |
|---|---|---|
| `bayer_dither` | hard | `levels=2`, `matrix=8` |
| `floyd_steinberg` | hard | `levels=2` |
| `threshold` | hard | `t=128` |
| `posterize` | tonal | `levels=4` |
| `hue_rotate` | tonal | `degrees=90` |
| `duotone` | tonal | `dark=r,g,b`, `light=r,g,b` |
| `pixel_sort` | spatial | `low=40`, `high=220`, `mode=interval\|row`, `vertical=0` |
| `rgb_split` | spatial | `shift=8` |
| `gaussian_blur` | tonal | `radius=5` |

Add an effect: write `fn(image, **params) -> image` in `effects.py` and decorate
with `@effect("name", hard=True/False)`. It's instantly available in the CLI.
