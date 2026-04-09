#!/usr/bin/env python3
"""
Export an AdaIN (Adaptive Instance Normalization) style transfer model to ONNX.

Uses pretrained VGG19 encoder + decoder weights from:
  https://github.com/naoto0804/pytorch-AdaIN

Usage:
    pip install torch torchvision onnx onnxruntime
    python export_adain_onnx.py

Downloads pretrained weights automatically on first run.
Outputs: assets/models/adain_style_transfer.onnx

Optional verification:
    python export_adain_onnx.py --verify content.jpg style.jpg
"""

import argparse
import os
import sys
from pathlib import Path
from urllib.request import urlretrieve

import torch
import torch.nn as nn
import torch.nn.functional as F

# ─── Pretrained weight URLs (from naoto0804/pytorch-AdaIN releases) ───

VGG_URL = "https://github.com/naoto0804/pytorch-AdaIN/releases/download/v0.0.0/vgg_normalised.pth"
DECODER_URL = "https://github.com/naoto0804/pytorch-AdaIN/releases/download/v0.0.0/decoder.pth"

WEIGHTS_DIR = Path(__file__).parent / "weights"
ASSETS_DIR = Path(__file__).parent.parent / "assets" / "models"


# ─── VGG19 Encoder (up to relu4_1) ───

class VGGEncoder(nn.Module):
    """VGG19 encoder up to relu4_1, with normalized weights."""

    def __init__(self):
        super().__init__()
        # Matches the naoto0804 vgg_normalised.pth layer ordering
        self.layers = nn.Sequential(
            nn.Conv2d(3, 3, 1),        # 0: normalize
            nn.ReflectionPad2d(1),     # 1
            nn.Conv2d(3, 64, 3),       # 2
            nn.ReLU(inplace=True),     # 3: relu1_1
            nn.ReflectionPad2d(1),     # 4
            nn.Conv2d(64, 64, 3),      # 5
            nn.ReLU(inplace=True),     # 6: relu1_2
            nn.MaxPool2d(2, 2),        # 7
            nn.ReflectionPad2d(1),     # 8
            nn.Conv2d(64, 128, 3),     # 9
            nn.ReLU(inplace=True),     # 10: relu2_1
            nn.ReflectionPad2d(1),     # 11
            nn.Conv2d(128, 128, 3),    # 12
            nn.ReLU(inplace=True),     # 13: relu2_2
            nn.MaxPool2d(2, 2),        # 14
            nn.ReflectionPad2d(1),     # 15
            nn.Conv2d(128, 256, 3),    # 16
            nn.ReLU(inplace=True),     # 17: relu3_1
            nn.ReflectionPad2d(1),     # 18
            nn.Conv2d(256, 256, 3),    # 19
            nn.ReLU(inplace=True),     # 20: relu3_2
            nn.ReflectionPad2d(1),     # 21
            nn.Conv2d(256, 256, 3),    # 22
            nn.ReLU(inplace=True),     # 23: relu3_3
            nn.ReflectionPad2d(1),     # 24
            nn.Conv2d(256, 256, 3),    # 25
            nn.ReLU(inplace=True),     # 26: relu3_4
            nn.MaxPool2d(2, 2),        # 27
            nn.ReflectionPad2d(1),     # 28
            nn.Conv2d(256, 512, 3),    # 29
            nn.ReLU(inplace=True),     # 30: relu4_1
        )

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.layers(x)


# ─── Decoder (mirrors encoder, upsamples back to image) ───

class Decoder(nn.Module):
    """Decoder that mirrors VGG encoder from relu4_1 back to RGB."""

    def __init__(self):
        super().__init__()
        self.layers = nn.Sequential(
            nn.ReflectionPad2d(1),
            nn.Conv2d(512, 256, 3),
            nn.ReLU(inplace=True),
            nn.Upsample(scale_factor=2, mode="nearest"),
            nn.ReflectionPad2d(1),
            nn.Conv2d(256, 256, 3),
            nn.ReLU(inplace=True),
            nn.ReflectionPad2d(1),
            nn.Conv2d(256, 256, 3),
            nn.ReLU(inplace=True),
            nn.ReflectionPad2d(1),
            nn.Conv2d(256, 256, 3),
            nn.ReLU(inplace=True),
            nn.ReflectionPad2d(1),
            nn.Conv2d(256, 128, 3),
            nn.ReLU(inplace=True),
            nn.Upsample(scale_factor=2, mode="nearest"),
            nn.ReflectionPad2d(1),
            nn.Conv2d(128, 128, 3),
            nn.ReLU(inplace=True),
            nn.ReflectionPad2d(1),
            nn.Conv2d(128, 64, 3),
            nn.ReLU(inplace=True),
            nn.Upsample(scale_factor=2, mode="nearest"),
            nn.ReflectionPad2d(1),
            nn.Conv2d(64, 64, 3),
            nn.ReLU(inplace=True),
            nn.ReflectionPad2d(1),
            nn.Conv2d(64, 3, 3),
        )

    def forward(self, x: torch.Tensor) -> torch.Tensor:
        return self.layers(x)


# ─── AdaIN operation ───

def adaptive_instance_normalization(
    content_feat: torch.Tensor, style_feat: torch.Tensor
) -> torch.Tensor:
    """Adaptive Instance Normalization.

    Adjusts content features to have the same channel-wise mean and variance
    as style features.
    """
    size = content_feat.size()
    style_mean = style_feat.mean(dim=[2, 3], keepdim=True)
    style_std = style_feat.std(dim=[2, 3], keepdim=True) + 1e-5
    content_mean = content_feat.mean(dim=[2, 3], keepdim=True)
    content_std = content_feat.std(dim=[2, 3], keepdim=True) + 1e-5
    normalized = (content_feat - content_mean) / content_std
    return normalized * style_std + style_mean


# ─── Combined model for ONNX export ───

class AdaINStyleTransfer(nn.Module):
    """Full AdaIN pipeline: encode both images, normalize, decode.

    Inputs:
        content: [1, 3, H, W] — normalized RGB content image
        style:   [1, 3, H, W] — normalized RGB style image

    Output:
        styled:  [1, 3, H, W] — stylized RGB image
    """

    def __init__(self, encoder: VGGEncoder, decoder: Decoder):
        super().__init__()
        self.encoder = encoder
        self.decoder = decoder

    def forward(
        self, content: torch.Tensor, style: torch.Tensor
    ) -> torch.Tensor:
        content_feat = self.encoder(content)
        style_feat = self.encoder(style)
        normalized = adaptive_instance_normalization(content_feat, style_feat)
        return self.decoder(normalized)


# ─── Weight downloading ───

def download_weights():
    """Download pretrained weights if not already present."""
    WEIGHTS_DIR.mkdir(parents=True, exist_ok=True)

    vgg_path = WEIGHTS_DIR / "vgg_normalised.pth"
    decoder_path = WEIGHTS_DIR / "decoder.pth"

    if not vgg_path.exists():
        print(f"Downloading VGG encoder weights to {vgg_path} ...")
        urlretrieve(VGG_URL, vgg_path)
        print("  Done.")

    if not decoder_path.exists():
        print(f"Downloading decoder weights to {decoder_path} ...")
        urlretrieve(DECODER_URL, decoder_path)
        print("  Done.")

    return vgg_path, decoder_path


# ─── Build and load model ───

def build_model() -> AdaINStyleTransfer:
    """Build the AdaIN model and load pretrained weights."""
    vgg_path, decoder_path = download_weights()

    encoder = VGGEncoder()
    decoder = Decoder()

    # Load VGG encoder weights (naoto0804 format: Sequential state_dict).
    # The weights file contains the full VGG up to relu5_1 but our encoder
    # only goes to relu4_1, so use strict=False to ignore extra layers.
    vgg_state = torch.load(vgg_path, map_location="cpu", weights_only=True)
    encoder.layers.load_state_dict(vgg_state, strict=False)

    # Load decoder weights
    decoder_state = torch.load(decoder_path, map_location="cpu", weights_only=True)
    decoder.layers.load_state_dict(decoder_state)

    model = AdaINStyleTransfer(encoder, decoder)
    model.eval()
    return model


# ─── ONNX export ───

def export_onnx(model: AdaINStyleTransfer, output_path: Path, size: int = 256):
    """Export the model to ONNX with fixed spatial dimensions.

    CoreML (Apple Neural Engine) requires fixed input shapes — dynamic
    axes cause zero-dim compilation failures. We export separate models
    for each resolution (256, 512).
    """
    output_path.parent.mkdir(parents=True, exist_ok=True)

    # Dummy inputs at the exact target size (no dynamic axes).
    content = torch.randn(1, 3, size, size)
    style = torch.randn(1, 3, size, size)

    torch.onnx.export(
        model,
        (content, style),
        str(output_path),
        input_names=["content", "style"],
        output_names=["output"],
        opset_version=17,
        do_constant_folding=True,
        dynamo=False,
    )
    print(f"Exported ONNX model to {output_path}")
    print(f"  File size: {output_path.stat().st_size / 1024 / 1024:.1f} MB")


# ─── Verification ───

def verify(model: AdaINStyleTransfer, content_path: str, style_path: str):
    """Run inference with PyTorch and ONNX Runtime, compare outputs."""
    import numpy as np
    from PIL import Image

    try:
        import onnxruntime as ort
    except ImportError:
        print("Install onnxruntime for verification: pip install onnxruntime")
        return

    def load_image(path: str, size: int = 256) -> torch.Tensor:
        img = Image.open(path).convert("RGB").resize((size, size))
        arr = np.array(img, dtype=np.float32) / 255.0
        return torch.from_numpy(arr).permute(2, 0, 1).unsqueeze(0)

    content = load_image(content_path)
    style = load_image(style_path)

    # PyTorch reference
    with torch.no_grad():
        pt_output = model(content, style).numpy()

    # ONNX Runtime
    onnx_path = ASSETS_DIR / "adain_style_transfer.onnx"
    session = ort.InferenceSession(str(onnx_path))
    ort_output = session.run(
        None,
        {"content": content.numpy(), "style": style.numpy()},
    )[0]

    # Compare
    max_diff = np.abs(pt_output - ort_output).max()
    mean_diff = np.abs(pt_output - ort_output).mean()
    print(f"Verification: max_diff={max_diff:.6f}, mean_diff={mean_diff:.6f}")
    if max_diff < 1e-4:
        print("  PASS: ONNX output matches PyTorch output.")
    else:
        print("  WARNING: Outputs differ. Check model export.")

    # Save output image for visual inspection
    out_img = (np.clip(ort_output[0].transpose(1, 2, 0), 0, 1) * 255).astype(
        np.uint8
    )
    out_path = ASSETS_DIR / "verification_output.png"
    Image.fromarray(out_img).save(out_path)
    print(f"  Saved verification output to {out_path}")


# ─── Main ───

def main():
    parser = argparse.ArgumentParser(description="Export AdaIN to ONNX")
    parser.add_argument(
        "--verify",
        nargs=2,
        metavar=("CONTENT", "STYLE"),
        help="Verify ONNX output against PyTorch with given images",
    )
    parser.add_argument(
        "--size",
        type=int,
        default=256,
        help="Export trace size (default 256, model supports dynamic sizes)",
    )
    args = parser.parse_args()

    model = build_model()

    # Export fixed-size models for CoreML compatibility.
    for size in [256, 512]:
        output_path = ASSETS_DIR / f"adain_style_transfer_{size}.onnx"
        export_onnx(model, output_path, size=size)

    if args.verify:
        verify(model, args.verify[0], args.verify[1])


if __name__ == "__main__":
    main()
