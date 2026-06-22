"""Effect registry.

Every effect shares one signature:

    fn(image: np.ndarray, **params) -> np.ndarray

where `image` is an HxWx3 uint8 RGB array and the return is the same shape/dtype.
Effects process the FULL image; compositing through a mask happens in render.py.

Register with @effect("name"). HARD_EFFECTS naturally want the tight binary
mask (crisp edges); everything else defaults to the soft matte. render.py reads
these sets to pick the default mask, and the user can override per-apply.
"""
from __future__ import annotations

import numpy as np

REGISTRY: dict[str, callable] = {}
# Effects whose look depends on hard edges -> default to the binary mask.
HARD_EFFECTS: set[str] = set()


def effect(name: str, hard: bool = False):
    def deco(fn):
        REGISTRY[name] = fn
        if hard:
            HARD_EFFECTS.add(name)
        return fn
    return deco


def list_effects() -> list[str]:
    return sorted(REGISTRY)


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------
def _luma(img: np.ndarray) -> np.ndarray:
    """Rec.601 luminance, float32 0..255."""
    f = img.astype(np.float32)
    return 0.299 * f[..., 0] + 0.587 * f[..., 1] + 0.114 * f[..., 2]


def _f(name, params, default):
    return float(params.get(name, default))


def _i(name, params, default):
    return int(params.get(name, default))


# ---------------------------------------------------------------------------
# dithering
# ---------------------------------------------------------------------------
def _bayer_matrix(n: int) -> np.ndarray:
    """Recursive Bayer threshold matrix, normalized to (0,1)."""
    m = np.array([[0, 2], [3, 1]], dtype=np.float32)
    size = 2
    while size < n:
        m = np.block([
            [4 * m + 0, 4 * m + 2],
            [4 * m + 3, 4 * m + 1],
        ])
        size *= 2
    return (m + 0.5) / (size * size)


@effect("bayer_dither", hard=True)
def bayer_dither(image, **p):
    """Ordered (Bayer) dither. levels>=2 per channel; matrix=2/4/8."""
    levels = max(2, _i("levels", p, 2))
    n = _i("matrix", p, 8)
    thr = _bayer_matrix(n)
    h, w, _ = image.shape
    tile = np.tile(thr, (h // n + 1, w // n + 1))[:h, :w]
    f = image.astype(np.float32) / 255.0
    out = np.empty_like(f)
    for c in range(3):
        scaled = f[..., c] * (levels - 1)
        lo = np.floor(scaled)
        frac = scaled - lo
        out[..., c] = (lo + (frac > tile)) / (levels - 1)
    return np.clip(out * 255.0, 0, 255).astype(np.uint8)


@effect("floyd_steinberg", hard=True)
def floyd_steinberg(image, **p):
    """Floyd–Steinberg error diffusion, levels>=2 per channel."""
    levels = max(2, _i("levels", p, 2))
    f = image.astype(np.float32) / 255.0
    h, w, _ = f.shape
    q = levels - 1
    for y in range(h):
        for x in range(w):
            old = f[y, x].copy()
            new = np.round(old * q) / q
            f[y, x] = new
            err = old - new
            if x + 1 < w:
                f[y, x + 1] += err * 7 / 16
            if y + 1 < h:
                if x > 0:
                    f[y + 1, x - 1] += err * 3 / 16
                f[y + 1, x] += err * 5 / 16
                if x + 1 < w:
                    f[y + 1, x + 1] += err * 1 / 16
    return np.clip(f * 255.0, 0, 255).astype(np.uint8)


@effect("threshold", hard=True)
def threshold(image, **p):
    """1-bit luminance threshold -> black/white. t in 0..255."""
    t = _f("t", p, 128.0)
    mask = _luma(image) >= t
    out = np.where(mask[..., None], 255, 0).astype(np.uint8)
    return np.repeat(out, 3, axis=2) if out.shape[2] == 1 else out


# ---------------------------------------------------------------------------
# tonal / color
# ---------------------------------------------------------------------------
@effect("posterize")
def posterize(image, **p):
    """Quantize each channel to N levels."""
    levels = max(2, _i("levels", p, 4))
    f = image.astype(np.float32) / 255.0
    q = np.round(f * (levels - 1)) / (levels - 1)
    return np.clip(q * 255.0, 0, 255).astype(np.uint8)


@effect("hue_rotate")
def hue_rotate(image, **p):
    """Rotate hue by `degrees`."""
    import colorsys  # noqa: keep stdlib-only path explicit
    deg = _f("degrees", p, 90.0)
    import cv2
    hsv = cv2.cvtColor(image, cv2.COLOR_RGB2HSV).astype(np.float32)
    hsv[..., 0] = (hsv[..., 0] + deg / 2.0) % 180.0  # OpenCV H is 0..180
    return cv2.cvtColor(hsv.astype(np.uint8), cv2.COLOR_HSV2RGB)


@effect("duotone")
def duotone(image, **p):
    """Map luminance onto a ramp between two RGB colors.
    dark='r,g,b' light='r,g,b' (0..255)."""
    def parse(key, default):
        v = p.get(key)
        if v is None:
            return np.array(default, dtype=np.float32)
        return np.array([float(x) for x in str(v).split(",")], dtype=np.float32)
    dark = parse("dark", (20, 12, 60))
    light = parse("light", (255, 200, 90))
    t = (_luma(image) / 255.0)[..., None]
    out = dark * (1 - t) + light * t
    return np.clip(out, 0, 255).astype(np.uint8)


# ---------------------------------------------------------------------------
# glitch / spatial
# ---------------------------------------------------------------------------
@effect("pixel_sort")
def pixel_sort(image, **p):
    """Threshold-gated pixel sort along scanlines.

    Sorts contiguous runs whose luminance falls in [low, high] by luminance.
    mode='row' sorts whole rows (cruder). vertical=1 sorts columns.
    """
    low = _f("low", p, 40.0)
    high = _f("high", p, 220.0)
    mode = str(p.get("mode", "interval"))
    vertical = _i("vertical", p, 0)

    img = np.transpose(image, (1, 0, 2)) if vertical else image.copy()
    lum = _luma(img)
    out = img.copy()
    h = img.shape[0]
    for y in range(h):
        row = img[y]
        l = lum[y]
        if mode == "row":
            order = np.argsort(l)
            out[y] = row[order]
            continue
        gate = (l >= low) & (l <= high)
        x = 0
        n = len(gate)
        while x < n:
            if gate[x]:
                x2 = x
                while x2 < n and gate[x2]:
                    x2 += 1
                seg = row[x:x2]
                order = np.argsort(l[x:x2])
                out[y, x:x2] = seg[order]
                x = x2
            else:
                x += 1
    return np.transpose(out, (1, 0, 2)) if vertical else out


@effect("rgb_split")
def rgb_split(image, **p):
    """Offset R and B channels horizontally (chromatic aberration)."""
    shift = _i("shift", p, 8)
    out = image.copy()
    out[..., 0] = np.roll(image[..., 0], shift, axis=1)
    out[..., 2] = np.roll(image[..., 2], -shift, axis=1)
    return out


@effect("gaussian_blur")
def gaussian_blur(image, **p):
    """Gaussian blur. radius -> kernel size."""
    import cv2
    radius = max(1, _i("radius", p, 5))
    k = radius * 2 + 1
    return cv2.GaussianBlur(image, (k, k), 0)
