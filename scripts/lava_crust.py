"""Make the LavaCrust material in assets/materials from a rock scan: the
scan's colour, normal, roughness and height pictures, and an emission
picture baked from its height, so lava's crust is dark plates with molten
rock glowing in the seams between them. A seam is a spot lower than the
ground round it (the height under a blur of it): the deepest seams glow
yellow, shallower ones orange to dark red, and the broad low patches,
where the crust is thinnest, a faint red. Writes the pictures and a
MaterialX file in ambientCG's layout, and copies the scan's preview.
Usage: lava_crust.py SCAN_DIR SCAN_ID OUT_DIR NAME"""
import os, shutil, sys
import numpy as np
from PIL import Image, ImageFilter

scan, scan_id, out, name = sys.argv[1:5]
os.makedirs(out, exist_ok=True)
prefix = f"{name}_2K-JPG"
for kind in ("Color", "NormalGL", "Roughness", "Displacement"):
    shutil.copy(os.path.join(scan, f"{scan_id}_2K-JPG_{kind}.jpg"), os.path.join(out, f"{prefix}_{kind}.jpg"))
shutil.copy(os.path.join(scan, f"{scan_id}.png"), os.path.join(out, "preview.png"))

heights = Image.open(os.path.join(scan, f"{scan_id}_2K-JPG_Displacement.jpg")).convert("L")
h = np.asarray(heights, np.float32) / 255.0
around = np.asarray(heights.filter(ImageFilter.GaussianBlur(14)), np.float32) / 255.0
below = around - h
lo, hi = np.quantile(below, [0.80, 0.995])
glow = np.clip((below - lo) / (hi - lo), 0.0, 1.0) ** 1.3
thin = np.clip((np.quantile(h, 0.15) - h) / 0.1, 0.0, 1.0) * 0.12
glow = np.maximum(glow, thin)[..., None]
dark_red = np.array([0.30, 0.015, 0.0])
orange = np.array([1.0, 0.25, 0.02])
yellow = np.array([1.0, 0.75, 0.32])
colour = np.where(glow < 0.55, dark_red + (orange - dark_red) * (glow / 0.55),
                  orange + (yellow - orange) * ((glow - 0.55) / 0.45))
colour *= np.clip(glow * 2.0, 0.0, 1.0)
pixels = (np.clip(colour, 0.0, 1.0) ** (1 / 2.2) * 255.0 + 0.5).astype(np.uint8)
Image.fromarray(pixels, "RGB").save(os.path.join(out, f"{prefix}_Emission.jpg"), quality=92)

node = prefix.replace("-", "_")
mtlx = f"""<?xml version="1.0"?>
<materialx version="1.39" fileprefix="./">
  <open_pbr_surface type="surfaceshader" name="{node}_OpenPbrSurface">
    <input nodename="{node}_Color" type="color3" name="base_color" />
    <input nodename="NormalMap" type="vector3" name="geometry_normal" />
    <input nodename="{node}_Roughness" type="float" name="specular_roughness" />
    <input nodename="{node}_Emission" type="color3" name="emission_color" />
    <input type="float" name="emission_luminance" value="1.0" />
  </open_pbr_surface>
  <surfacematerial type="material" name="{node}">
    <input nodename="{node}_OpenPbrSurface" type="surfaceshader" name="surfaceshader" />
    <input nodename="displacement" type="displacementshader" name="displacementshader" />
  </surfacematerial>
  <hextiledimage type="color3" name="{node}_Color">
    <input value="{prefix}_Color.jpg" colorspace="srgb_texture" type="filename" name="file" />
    <input value="1.0, 1.0" type="vector2" name="tiling" />
    <input name="realworldimagesize" type="vector2" value="1.0, 1.0" unit="meter" unittype="distance" />
  </hextiledimage>
  <tiledimage type="color3" name="{node}_Emission">
    <input value="{prefix}_Emission.jpg" colorspace="srgb_texture" type="filename" name="file" />
    <input value="1.0, 1.0" type="vector2" name="uvtiling" />
  </tiledimage>
  <tiledimage type="float" name="{node}_Displacement">
    <input value="{prefix}_Displacement.jpg" type="filename" name="file" />
    <input value="1.0, 1.0" type="vector2" name="uvtiling" />
  </tiledimage>
  <displacement type="displacementshader" name="displacement">
    <input nodename="{node}_Displacement" type="float" name="displacement" />
    <input name="scale" type="float" value="0.03" unit="meter" unittype="distance" />
  </displacement>
  <tiledimage type="vector3" name="{node}_NormalGL">
    <input value="{prefix}_NormalGL.jpg" type="filename" name="file" />
    <input value="1.0, 1.0" type="vector2" name="uvtiling" />
  </tiledimage>
  <normalmap type="vector3" name="NormalMap">
    <input nodename="{node}_NormalGL" type="vector3" name="in" />
    <input value="1.0" type="float" name="scale" />
  </normalmap>
  <tiledimage type="float" name="{node}_Roughness">
    <input value="{prefix}_Roughness.jpg" type="filename" name="file" />
    <input value="1.0, 1.0" type="vector2" name="uvtiling" />
  </tiledimage>
</materialx>
"""
with open(os.path.join(out, f"{prefix}.mtlx"), "w") as f:
    f.write(mtlx)
