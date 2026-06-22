"""Mask boundary refinement: turn a tight binary mask into a soft alpha matte.

Two backends:
  - "guided"  : guided filter (fast, robust, default). Implemented with box
                filters so it needs only numpy + opencv core.
  - "pymatting": closed-form alpha matting (slower, softer on wispy edges like
                 petals). Auto-downscales for the linear solve, then upsamples.
"""
from __future__ import annotations

import cv2
import numpy as np


def _box(img: np.ndarray, r: int) -> np.ndarray:
    return cv2.boxFilter(img, ddepth=-1, ksize=(2 * r + 1, 2 * r + 1),
                         normalize=True, borderType=cv2.BORDER_REFLECT)


def guided_filter(guide: np.ndarray, src: np.ndarray, r: int = 8,
                  eps: float = 1e-4) -> np.ndarray:
    """Edge-aware smoothing of `src` guided by grayscale `guide` (both float32 0..1)."""
    mean_i = _box(guide, r)
    mean_p = _box(src, r)
    corr_i = _box(guide * guide, r)
    corr_ip = _box(guide * src, r)
    var_i = corr_i - mean_i * mean_i
    cov_ip = corr_ip - mean_i * mean_p
    a = cov_ip / (var_i + eps)
    b = mean_p - a * mean_i
    return _box(a, r) * guide + _box(b, r)


def make_trimap(binary: np.ndarray, band: int = 12) -> np.ndarray:
    """0=bg, 1=fg, 0.5=unknown band around the edge."""
    b = (binary > 0).astype(np.uint8)
    k = np.ones((band * 2 + 1, band * 2 + 1), np.uint8)
    fg = cv2.erode(b, k)
    bg = cv2.dilate(b, k)
    tri = np.full(b.shape, 0.5, np.float32)
    tri[bg == 0] = 0.0
    tri[fg == 1] = 1.0
    return tri


def refine(image_rgb: np.ndarray, binary: np.ndarray, backend: str = "guided",
           band: int = 12) -> np.ndarray:
    """Return a soft alpha (float32 HxW, 0..1) for one instance."""
    if backend == "pymatting":
        return _pymatting(image_rgb, binary, band)
    guide = cv2.cvtColor(image_rgb, cv2.COLOR_RGB2GRAY).astype(np.float32) / 255.0
    src = (binary > 0).astype(np.float32)
    alpha = guided_filter(guide, src, r=band, eps=1e-3)
    return np.clip(alpha, 0.0, 1.0)


def _pymatting(image_rgb: np.ndarray, binary: np.ndarray, band: int) -> np.ndarray:
    from pymatting import estimate_alpha_cf
    h, w = binary.shape
    scale = min(1.0, 1024.0 / max(h, w))
    if scale < 1.0:
        sw, sh = int(w * scale), int(h * scale)
        img_s = cv2.resize(image_rgb, (sw, sh), interpolation=cv2.INTER_AREA)
        bin_s = cv2.resize(binary, (sw, sh), interpolation=cv2.INTER_NEAREST)
    else:
        img_s, bin_s = image_rgb, binary
    tri = make_trimap(bin_s, band=max(2, int(band * scale)))
    alpha = estimate_alpha_cf(img_s.astype(np.float64) / 255.0, tri.astype(np.float64))
    alpha = alpha.astype(np.float32)
    if scale < 1.0:
        alpha = cv2.resize(alpha, (w, h), interpolation=cv2.INTER_LINEAR)
    return np.clip(alpha, 0.0, 1.0)
