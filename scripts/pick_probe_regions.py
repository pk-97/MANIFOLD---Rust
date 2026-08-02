#!/usr/bin/env python3
"""
Analyze RT captures to pick probe regions for sharpness and halo measurements.

Uses gradient magnitude to find edges in composite channel, then:
1. For sharpness: find hard shadow boundaries (high gradient)
2. For halo: find silhouette edges against void background

Records chosen regions + coordinates in JSON format.
"""

import numpy as np
from PIL import Image
import json
import sys
from pathlib import Path

def find_edges_by_gradient(image_array, threshold_percentile=95):
    """
    Find edges using gradient magnitude (Sobel-like).
    Returns edge pixels above threshold percentile.
    """
    # Simple gradient approximation
    grad_x = np.diff(image_array.astype(float), axis=1)
    grad_y = np.diff(image_array.astype(float), axis=0)

    # Pad to match original size
    grad_x = np.pad(grad_x, ((0, 0), (0, 1)), mode='constant')
    grad_y = np.pad(grad_y, ((0, 1), (0, 0)), mode='constant')

    # Gradient magnitude
    grad_mag = np.sqrt(grad_x**2 + grad_y**2)

    # Threshold at percentile
    threshold = np.percentile(grad_mag, threshold_percentile)
    edges = grad_mag > threshold

    return edges, grad_mag, threshold

def analyze_shadow_boundary(image_array, edges, x_start, y_start, width=100):
    """
    Analyze a candidate shadow boundary region.
    Returns transition width estimate.
    """
    region = image_array[y_start:y_start+width, x_start:x_start+width]
    edge_region = edges[y_start:y_start+width, x_start:x_start+width]

    if not edge_region.any():
        return None

    # Find the strongest edge in the region
    edge_y, edge_x = np.unravel_index(np.argmax(region * edge_region), region.shape)

    # Sample perpendicular to the edge (horizontal line through the edge point)
    line = region[edge_y, :]

    # Find transition width (distance between 10% and 90% of the range)
    line_min, line_max = line.min(), line.max()
    if line_max - line_min < 0.05:  # Low contrast edge
        return None

    threshold_10 = line_min + 0.1 * (line_max - line_min)
    threshold_90 = line_min + 0.9 * (line_max - line_min)

    below_10 = line < threshold_10
    above_90 = line > threshold_90

    if not below_10.any() or not above_90.any():
        return None

    left_10 = np.where(below_10)[0][-1]  # Last pixel below 10%
    right_90 = np.where(above_90)[0][0]   # First pixel above 90%

    transition_width = abs(right_90 - left_10)
    return transition_width, (edge_x, edge_y), (line_min, line_max)

def analyze_silhouette_halo(image_array, edges, x_start, y_start, width=50):
    """
    Analyze a candidate silhouette edge for halo bleed.
    Measures how far luminance extends into the void side.
    """
    region = image_array[y_start:y_start+width, x_start:x_start+width]
    edge_region = edges[y_start:y_start+width, x_start:x_start+width]

    if not edge_region.any():
        return None

    # Find edge center
    edge_y, edge_x = np.unravel_index(np.argmax(region * edge_region), region.shape)

    # Sample horizontal line through edge
    line = region[edge_y, :]

    # Find the edge point (maximum gradient in line)
    line_grad = np.abs(np.diff(line.astype(float)))
    if len(line_grad) == 0:
        return None

    edge_pos = np.argmax(line_grad)
    edge_value = line[edge_pos]

    # Look at void side (right side for left-facing silhouette)
    # Need to determine which side is void (darker)
    left_mean = np.mean(line[max(0, edge_pos-10):edge_pos])
    right_mean = np.mean(line[edge_pos:min(len(line), edge_pos+10)])

    void_side = "left" if left_mean < right_mean else "right"

    # Measure how far luminance extends into void side
    # Use 5% of edge contrast as threshold
    contrast = abs(left_mean - right_mean)
    threshold = max(left_mean, right_mean) - 0.05 * contrast

    if void_side == "left":
        void_pixels = line[edge_pos::-1]  # Reverse from edge to left
    else:
        void_pixels = line[edge_pos:]     # From edge to right

    bleed_pixels = 0
    for val in void_pixels:
        if val > threshold:
            bleed_pixels += 1
        else:
            break

    return bleed_pixels, (edge_x, edge_y), void_side, (left_mean, right_mean)

def pick_probe_regions(composite_path, max_regions=5):
    """
    Pick probe regions from a composite capture.
    Returns list of (region_type, x, y, width, height, rationale).
    """
    img = Image.open(composite_path)
    img_gray = img.convert('L')
    img_array = np.array(img_gray).astype(float) / 255.0

    h, w = img_array.shape

    # Find edges
    edges, grad_mag, threshold = find_edges_by_gradient(img_array)

    # Get edge coordinates
    edge_coords = np.argwhere(edges)

    if len(edge_coords) == 0:
        return []

    regions = []

    # Cluster edges into regions (simple spatial clustering)
    # Use a grid-based approach for simplicity
    grid_size = 200
    for y in range(0, h - grid_size, grid_size):
        for x in range(0, w - grid_size, grid_size):
            region_edges = 0
            total_grad = 0

            for ey, ex in edge_coords:
                if y <= ey < y + grid_size and x <= ex < x + grid_size:
                    region_edges += 1
                    total_grad += grad_mag[ey, ex]

            if region_edges > 50:  # Minimum edge density
                strength = total_grad / max(region_edges, 1)

                # Analyze this region
                analysis = analyze_shadow_boundary(img_array, edges, x, y, width=min(grid_size, 100))
                if analysis and len(analysis) == 3:
                    width_est, center, contrast_range = analysis
                    regions.append({
                        "type": "shadow_boundary",
                        "x": int(x),
                        "y": int(y),
                        "width": int(min(grid_size, 100)),
                        "height": int(min(grid_size, 100)),
                        "center": [int(center[0]), int(center[1])],
                        "transition_width_estimate": float(width_est),
                        "contrast_range": [float(contrast_range[0]), float(contrast_range[1])],
                        "edge_strength": float(strength),
                        "rationale": f"Hard shadow boundary at ({x}, {y}), contrast {contrast_range[1]:.2f}-{contrast_range[0]:.2f}"
                    })

                if len(regions) >= max_regions:
                    return regions

    return regions

def main():
    if len(sys.argv) < 2:
        print("Usage: pick_probe_regions <composite.png> [output.json]")
        sys.exit(1)

    composite_path = Path(sys.argv[1])
    if not composite_path.exists():
        print(f"Error: {composite_path} not found")
        sys.exit(1)

    regions = pick_probe_regions(composite_path)

    output_file = Path(sys.argv[2]) if len(sys.argv) > 2 else None

    result = {
        "composite_file": str(composite_path),
        "image_size": list(Image.open(composite_path).size),
        "probe_regions": regions
    }

    if output_file:
        output_file.write_text(json.dumps(result, indent=2))
        print(f"Probe regions written to {output_file}")
    else:
        print(json.dumps(result, indent=2))

if __name__ == "__main__":
    main()