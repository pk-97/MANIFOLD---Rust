"""Draw the PineTwig cut-out material in assets/materials: a fir frond
from scratch, a stem from the bottom centre to the top, side shoots
alternating up it, and dense needles round all of them, in a dark
blue-green of real conifer albedo. Writes Color, Opacity, NormalGL,
Roughness and Displacement JPGs and a MaterialX file in ambientCG's
layout, and a preview. The seed is fixed, so it draws the same frond.
Usage: pine_twig.py OUT_DIR NAME"""
import os, sys
import numpy as np
from PIL import Image

N = 2048
out, name = sys.argv[1], sys.argv[2]
os.makedirs(out, exist_ok=True)
rng = np.random.default_rng(7)
height = np.zeros((N, N), np.float32)
shade = np.zeros((N, N), np.float32)  # 0 base of a needle .. 1 its tip
kind = np.zeros((N, N), np.float32)   # 1 needle, 0.5 stem
tone = np.zeros((N, N), np.float32)   # per needle colour jitter


def capsule(a, b, r, h, k, t=0.0, along=True):
    """Draw a round rod from a to b, radius r px, height h at its spine."""
    a = np.asarray(a, np.float32)
    b = np.asarray(b, np.float32)
    lo = np.floor(np.minimum(a, b) - r - 1).astype(int)
    hi = np.ceil(np.maximum(a, b) + r + 1).astype(int)
    lo = np.clip(lo, 0, N - 1)
    hi = np.clip(hi, 0, N - 1)
    if (hi <= lo).any():
        return
    ys, xs = np.mgrid[lo[1]:hi[1] + 1, lo[0]:hi[0] + 1].astype(np.float32)
    d = b - a
    ll = max(float(d @ d), 1e-6)
    s = np.clip(((xs - a[0]) * d[0] + (ys - a[1]) * d[1]) / ll, 0, 1)
    px = xs - (a[0] + s * d[0])
    py = ys - (a[1] + s * d[1])
    rr = r * (1.0 - 0.6 * s) if along else r
    dist = np.sqrt(px * px + py * py) / np.maximum(rr, 0.3)
    z = np.where(dist < 1, h * np.sqrt(np.clip(1 - dist * dist, 0, 1)), 0)
    region = (slice(lo[1], hi[1] + 1), slice(lo[0], hi[0] + 1))
    win = z > height[region]
    height[region] = np.where(win, z, height[region])
    shade[region] = np.where(win, s, shade[region])
    kind[region] = np.where(win, k, kind[region])
    tone[region] = np.where(win, t, tone[region])


def stem(p0, direction, length, width, needles, level):
    """A shoot: its spine, its needles both sides, and its own shoots."""
    p = np.asarray(p0, np.float32)
    d = np.asarray(direction, np.float32)
    d /= np.linalg.norm(d)
    steps = max(int(length / 6), 2)
    pts = [p.copy()]
    bend = rng.normal(0, 0.25) / steps
    for i in range(steps):
        ang = bend
        c, s_ = np.cos(ang), np.sin(ang)
        d = np.array([c * d[0] - s_ * d[1], s_ * d[0] + c * d[1]], np.float32)
        p = p + d * (length / steps)
        pts.append(p.copy())
    pts = np.array(pts)
    for i in range(len(pts) - 1):
        t = i / (len(pts) - 1)
        capsule(pts[i], pts[i + 1], width * (1 - 0.5 * t), 0.35, 0.5, 0.0, along=False)
    # Needles: dense pairs along the shoot, angled forward, shorter at the
    # tip, some across the stem so it reads full from the side.
    count = int(length / 1.1 * needles)
    for j in range(count):
        t = rng.uniform(0.02, 1.0)
        i = min(int(t * (len(pts) - 1)), len(pts) - 2)
        f = t * (len(pts) - 1) - i
        base = pts[i] + (pts[i + 1] - pts[i]) * f
        tangent = pts[i + 1] - pts[i]
        tangent /= np.linalg.norm(tangent) + 1e-6
        side = 1 if j % 2 == 0 else -1
        ang = np.radians(rng.uniform(35, 70)) * side
        c, s_ = np.cos(ang), np.sin(ang)
        nd = np.array([c * tangent[0] - s_ * tangent[1], s_ * tangent[0] + c * tangent[1]])
        nl = rng.uniform(0.75, 1.15) * NEEDLE * (1 - 0.45 * t ** 2) * (0.8 if level else 1.0)
        # Foreshortened needles pointing at or away from the viewer.
        nl *= rng.uniform(0.45, 1.0) if rng.random() < 0.35 else 1.0
        capsule(base, base + nd * nl, rng.uniform(2.2, 3.2), rng.uniform(0.6, 1.0), 1.0,
                rng.normal(0, 1))
    if level == 0:
        # Side shoots, alternating, leaning forward.
        k = 16
        for s_i in range(k):
            t = 0.06 + 0.86 * (s_i + rng.uniform(-0.2, 0.2)) / k
            i = min(int(t * (len(pts) - 1)), len(pts) - 2)
            base = pts[i]
            tangent = pts[i + 1] - pts[i]
            tangent /= np.linalg.norm(tangent)
            side = 1 if s_i % 2 == 0 else -1
            ang = np.radians(rng.uniform(40, 60)) * side
            c, s_ = np.cos(ang), np.sin(ang)
            nd = [c * tangent[0] - s_ * tangent[1], s_ * tangent[0] + c * tangent[1]]
            stem(base, nd, length * rng.uniform(0.36, 0.46) * (1 - 0.7 * t), width * 0.55, needles, 1)


NEEDLE = 95.0
stem((N / 2, N - 20), (rng.normal(0, 0.03), -1), N - 120, 7.0, 1.0, 0)

alpha = (height > 0).astype(np.float32)
# Colour: a dark blue-green fir, lighter toward needle tips and on fresh
# shoots, darker deep in; the stem brown.
base = np.array([0.022, 0.048, 0.024])
tip = np.array([0.06, 0.105, 0.045])
needle = base + (tip - base) * shade[..., None] ** 1.5
needle *= (1 + 0.12 * tone[..., None])
needle *= (0.75 + 0.25 * height[..., None])
bark = np.array([0.08, 0.05, 0.03])
color = np.where(kind[..., None] > 0.75, needle, bark)
color = np.clip(color, 0, 1) * alpha[..., None]
# Bleed colour into the clear texels so filtering never pulls in black.
from PIL import ImageFilter
srgb = np.where(color <= 0.0031308, color * 12.92, 1.055 * np.power(color, 1 / 2.4) - 0.055)
img = Image.fromarray((np.clip(srgb, 0, 1) * 255).astype(np.uint8))
mask = Image.fromarray((alpha * 255).astype(np.uint8))
fill = img.filter(ImageFilter.GaussianBlur(24))
m = mask.filter(ImageFilter.GaussianBlur(24))
fa = np.asarray(fill, np.float32) / np.maximum(np.asarray(m, np.float32)[..., None] / 255, 1e-3)
full = np.where(alpha[..., None] > 0, np.asarray(img, np.float32), np.clip(fa, 0, 255))
pre = f"{out}/{name}_2K-JPG"
Image.fromarray(full.astype(np.uint8)).save(f"{pre}_Color.jpg", quality=92)
mask.save(f"{pre}_Opacity.jpg", quality=95)
# Normals from the height of the rods, OpenGL green-up.
h = height * 3.0
gx = np.zeros_like(h)
gy = np.zeros_like(h)
gx[:, 1:-1] = (h[:, 2:] - h[:, :-2]) * 0.5
gy[1:-1, :] = (h[2:, :] - h[:-2, :]) * 0.5
n = np.stack([-gx, gy, np.ones_like(h)], -1)
n /= np.linalg.norm(n, axis=-1, keepdims=True)
Image.fromarray(((n * 0.5 + 0.5) * 255).astype(np.uint8)).save(f"{pre}_NormalGL.jpg", quality=95)
rough = np.where(kind > 0.75, 0.5 + 0.1 * (1 - shade), 0.85)
Image.fromarray((rough * 255).astype(np.uint8)).save(f"{pre}_Roughness.jpg", quality=95)
Image.fromarray((np.clip(height, 0, 1) * 255).astype(np.uint8)).save(f"{pre}_Displacement.jpg", quality=95)

M = f"{name}_2K_JPG"
F = f"{name}_2K-JPG"
mtlx = f"""<?xml version="1.0"?>
<materialx version="1.39" fileprefix="./">
  <open_pbr_surface type="surfaceshader" name="{M}_OpenPbrSurface">
    <input type="color3" name="base_color" nodename="{M}_Color" />
    <input type="vector3" name="geometry_normal" nodename="NormalMap" />
    <input type="float" name="geometry_opacity" nodename="{M}_Opacity" />
    <input type="float" name="specular_roughness" nodename="{M}_Roughness" />
  </open_pbr_surface>
  <surfacematerial type="material" name="{M}">
    <input type="surfaceshader" name="surfaceshader" nodename="{M}_OpenPbrSurface" />
    <input type="displacementshader" name="displacementshader" nodename="displacement" />
  </surfacematerial>
  <tiledimage type="color3" name="{M}_Color">
    <input type="filename" name="file" colorspace="srgb_texture" value="{F}_Color.jpg" />
    <input type="vector2" name="uvtiling" value="1.0, 1.0" />
  </tiledimage>
  <tiledimage type="float" name="{M}_Displacement">
    <input type="filename" name="file" value="{F}_Displacement.jpg" />
    <input type="vector2" name="uvtiling" value="1.0, 1.0" />
  </tiledimage>
  <displacement type="displacementshader" name="displacement">
    <input type="float" name="displacement" nodename="{M}_Displacement" />
    <input type="float" name="scale" value="1.0" />
  </displacement>
  <tiledimage type="vector3" name="{M}_NormalGL">
    <input type="filename" name="file" value="{F}_NormalGL.jpg" />
    <input type="vector2" name="uvtiling" value="1.0, 1.0" />
  </tiledimage>
  <normalmap type="vector3" name="NormalMap">
    <input type="vector3" name="in" nodename="{M}_NormalGL" />
    <input type="float" name="scale" value="1.0" />
  </normalmap>
  <tiledimage type="float" name="{M}_Opacity">
    <input type="filename" name="file" value="{F}_Opacity.jpg" />
    <input type="vector2" name="uvtiling" value="1.0, 1.0" />
  </tiledimage>
  <tiledimage type="float" name="{M}_Roughness">
    <input type="filename" name="file" value="{F}_Roughness.jpg" />
    <input type="vector2" name="uvtiling" value="1.0, 1.0" />
  </tiledimage>
</materialx>
"""
open(f"{pre}.mtlx", "w").write(mtlx)
Image.fromarray(full.astype(np.uint8)).resize((256, 256)).save(f"{out}/preview.png")
print(pre, alpha.mean())
