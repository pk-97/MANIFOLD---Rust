#!/usr/bin/env python3
"""Convert NIfTI (.nii / .nii.gz) volumes to .mrivol format for Manifold."""

import argparse
import json
import struct
import sys
from pathlib import Path

import nibabel as nib
import numpy as np


MAGIC = b"MRIVOL\x01\x00"


def convert(input_path: str, output_path: str, fmt: str = "R16") -> None:
    img = nib.load(input_path)

    # Reorient to canonical RAS (Right-Anterior-Superior) so all volumes
    # display in a consistent, upright orientation regardless of acquisition.
    img = nib.as_closest_canonical(img)

    data = img.get_fdata()
    header = img.header
    zooms = header.get_zooms()

    # Determine dimensions
    shape = data.shape
    if data.ndim == 3:
        dim_x, dim_y, dim_z = shape
        frames = 1
    elif data.ndim == 4:
        dim_x, dim_y, dim_z, frames = shape
    else:
        print(f"Error: expected 3D or 4D NIfTI, got {data.ndim}D", file=sys.stderr)
        sys.exit(1)

    voxel_spacing = [float(zooms[0]), float(zooms[1]), float(zooms[2])]
    vmin, vmax = float(data.min()), float(data.max())

    print(f"Input:   {input_path}")
    print(f"Shape:   {dim_x} x {dim_y} x {dim_z} x {frames} frames")
    print(f"Spacing: {voxel_spacing[0]:.3f} x {voxel_spacing[1]:.3f} x {voxel_spacing[2]:.3f} mm")
    print(f"Range:   [{vmin:.1f}, {vmax:.1f}]")
    print(f"Format:  {fmt}")

    # Build orientation matrix from affine (rotation part only, normalized)
    affine = img.affine[:3, :3]
    scales = np.sqrt((affine ** 2).sum(axis=0))
    scales[scales == 0] = 1.0
    rot = affine / scales
    orientation = rot.flatten().tolist()

    # For 4D data, move T from last axis to first so frames are contiguous.
    # NIfTI C-order on (X, Y, Z, T) interleaves frames; we need (T, X, Y, Z)
    # so that C-order gives T-outermost, Z-fastest-within-frame.
    if data.ndim == 4:
        data = np.moveaxis(data, 3, 0)  # (X, Y, Z, T) → (T, X, Y, Z)

    # Normalize data to [0, 1] for encoding
    if vmax > vmin:
        normalized = (data - vmin) / (vmax - vmin)
    else:
        normalized = np.zeros_like(data)

    # Encode voxels
    if fmt == "R8":
        voxels = (normalized * 255.0 + 0.5).astype(np.uint8)
        raw = voxels.tobytes(order="C")
    elif fmt == "R16":
        voxels = (normalized * 65535.0 + 0.5).astype(np.uint16)
        raw = voxels.astype("<u2").tobytes(order="C")
    elif fmt == "R32Float":
        voxels = normalized.astype(np.float32)
        raw = voxels.astype("<f4").tobytes(order="C")
    else:
        print(f"Error: unknown format {fmt}", file=sys.stderr)
        sys.exit(1)

    # Build JSON header
    vol_header = {
        "version": 1,
        "dim_x": int(dim_x),
        "dim_y": int(dim_y),
        "dim_z": int(dim_z),
        "frames": int(frames),
        "voxel_format": fmt,
        "voxel_range": [vmin, vmax],
        "voxel_spacing": voxel_spacing,
        "orientation": orientation,
        "description": f"Converted from {Path(input_path).name}",
    }
    header_json = json.dumps(vol_header, indent=None, separators=(",", ":")).encode("utf-8")

    # Write .mrivol
    with open(output_path, "wb") as f:
        f.write(MAGIC)
        f.write(struct.pack("<I", len(header_json)))
        f.write(header_json)
        f.write(raw)

    total_size = 8 + 4 + len(header_json) + len(raw)
    print(f"Output:  {output_path} ({total_size / 1024 / 1024:.1f} MB)")


def main():
    parser = argparse.ArgumentParser(description="Convert NIfTI to .mrivol")
    parser.add_argument("input", help="Input .nii or .nii.gz file")
    parser.add_argument("-o", "--output", help="Output .mrivol file (default: same name)")
    parser.add_argument(
        "-f", "--format",
        choices=["R8", "R16", "R32Float"],
        default="R16",
        help="Voxel format (default: R16)",
    )
    args = parser.parse_args()

    output = args.output or str(Path(args.input).with_suffix(".mrivol"))
    convert(args.input, output, args.format)


if __name__ == "__main__":
    main()
