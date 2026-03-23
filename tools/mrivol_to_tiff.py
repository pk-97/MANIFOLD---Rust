#!/usr/bin/env python3
"""Convert .mrivol volumes to per-axis TIFF slice directories.

For each axis (axial, sagittal, coronal), extracts every slice as a
16-bit grayscale TIFF with auto-windowing applied.

Usage:
    python tools/mrivol_to_tiff.py assets/mri-data/volumes/brain_250um_7T.mrivol
    python tools/mrivol_to_tiff.py assets/mri-data/volumes/brain_250um_7T.mrivol --axes axial
    python tools/mrivol_to_tiff.py assets/mri-data/volumes/brain_250um_7T.mrivol -o output/dir
"""

import argparse
import json
import struct
import sys
from pathlib import Path

import numpy as np
from PIL import Image

MAGIC = b"MRIVOL\x01\x00"
AXES = ["axial", "sagittal", "coronal"]


def load_mrivol(path: str) -> tuple[dict, np.ndarray]:
    """Load a .mrivol file, return (header, voxels_f32[x, y, z])."""
    data = Path(path).read_bytes()

    if len(data) < 12 or data[:8] != MAGIC:
        print(f"Error: not a valid .mrivol file: {path}", file=sys.stderr)
        sys.exit(1)

    header_len = struct.unpack_from("<I", data, 8)[0]
    header_end = 12 + header_len
    header = json.loads(data[12:header_end])

    dx, dy, dz = header["dim_x"], header["dim_y"], header["dim_z"]
    frames = header.get("frames", 1)
    fmt = header["voxel_format"]
    raw = data[header_end:]

    # Decode to float32 [0, 1]
    if fmt == "R8":
        voxels = np.frombuffer(raw, dtype=np.uint8).astype(np.float32) / 255.0
    elif fmt == "R16":
        voxels = np.frombuffer(raw, dtype="<u2").astype(np.float32) / 65535.0
    elif fmt == "R32Float":
        voxels = np.frombuffer(raw, dtype="<f4").copy()
    else:
        print(f"Error: unknown voxel format {fmt}", file=sys.stderr)
        sys.exit(1)

    # Reshape: file is stored as C-order (X, Y, Z) with Z fastest
    # For frame=0 only (use first frame for multi-frame volumes)
    frame_voxels = dx * dy * dz
    voxels = voxels[:frame_voxels].reshape((dx, dy, dz))

    print(f"Loaded: {path}")
    print(f"  Shape: {dx} x {dy} x {dz}, {frames} frame(s)")
    print(f"  Spacing: {header.get('voxel_spacing', [1, 1, 1])}")
    print(f"  Range: {header['voxel_range']}")

    return header, voxels


def compute_auto_window(voxels: np.ndarray) -> tuple[float, float]:
    """Compute 2nd-98th percentile window of non-zero voxels."""
    nonzero = voxels[voxels > 0.001]
    if len(nonzero) == 0:
        return 0.0, 1.0
    p2 = float(np.percentile(nonzero, 2))
    p98 = float(np.percentile(nonzero, 98))
    print(f"  Auto-window: [{p2:.4f}, {p98:.4f}]")
    return p2, p98


def extract_and_save_slices(
    voxels: np.ndarray,
    axis: str,
    output_dir: Path,
    window: tuple[float, float],
) -> None:
    """Extract slices along an axis and save as 16-bit TIFF."""
    w_low, w_high = window
    w_range = max(w_high - w_low, 0.001)

    # Map axis name to numpy axis index
    # voxels shape is (X, Y, Z) in NIfTI C-order
    # Axial = slice along Z, Sagittal = slice along X, Coronal = slice along Y
    if axis == "axial":
        n_slices = voxels.shape[2]
        get_slice = lambda i: voxels[:, :, i].T  # (Y, X) view
    elif axis == "sagittal":
        n_slices = voxels.shape[0]
        get_slice = lambda i: voxels[i, :, :].T  # (Z, Y) view
    elif axis == "coronal":
        n_slices = voxels.shape[1]
        get_slice = lambda i: voxels[:, i, :].T  # (Z, X) view
    else:
        print(f"Error: unknown axis {axis}", file=sys.stderr)
        sys.exit(1)

    axis_dir = output_dir / axis
    axis_dir.mkdir(parents=True, exist_ok=True)

    print(f"  {axis}: {n_slices} slices -> {axis_dir}/")

    for i in range(n_slices):
        sl = get_slice(i).astype(np.float32)

        # Apply window/level
        sl = (sl - w_low) / w_range
        np.clip(sl, 0.0, 1.0, out=sl)

        # Convert to 16-bit
        sl_u16 = (sl * 65535.0 + 0.5).astype(np.uint16)

        img = Image.fromarray(sl_u16, mode="I;16")
        img.save(axis_dir / f"{i:05d}.tiff", compression="tiff_lzw")

    print(f"    Done: {n_slices} TIFFs written")


def main():
    parser = argparse.ArgumentParser(
        description="Convert .mrivol to per-axis TIFF slices"
    )
    parser.add_argument("input", help="Input .mrivol file")
    parser.add_argument(
        "-o", "--output",
        help="Output directory (default: same name as input, without extension)",
    )
    parser.add_argument(
        "--axes",
        nargs="+",
        choices=AXES,
        default=AXES,
        help="Which axes to extract (default: all three)",
    )
    args = parser.parse_args()

    header, voxels = load_mrivol(args.input)
    window = compute_auto_window(voxels)

    output_dir = Path(args.output) if args.output else Path(args.input).with_suffix("")
    output_dir.mkdir(parents=True, exist_ok=True)
    print(f"Output: {output_dir}/")

    for axis in args.axes:
        extract_and_save_slices(voxels, axis, output_dir, window)

    print(f"\nDone. TIFF slices written to {output_dir}/")


if __name__ == "__main__":
    main()
