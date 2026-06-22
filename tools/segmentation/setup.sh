#!/usr/bin/env bash
# Create a venv and install all deps for the segmentation + effects pipeline.
# Model weights (~1.6 GB) download lazily on the first segment.py run, with a
# tqdm progress bar. Run with --weights to pre-download them now instead.
set -euo pipefail

cd "$(dirname "$0")"

PY="${PYTHON:-python3}"

if [ ! -d .venv ]; then
  echo ">> creating venv (.venv)"
  "$PY" -m venv .venv
fi

# shellcheck disable=SC1091
source .venv/bin/activate

echo ">> upgrading pip"
python -m pip install --upgrade pip

echo ">> installing requirements (pip shows a progress bar per package)"
pip install -r requirements.txt

if [ "${1:-}" = "--weights" ]; then
  echo ">> pre-downloading model weights (tqdm bars)"
  python - <<'PY'
from transformers import AutoModelForZeroShotObjectDetection, AutoProcessor
from sam2.sam2_image_predictor import SAM2ImagePredictor
print(">> Grounding DINO")
AutoProcessor.from_pretrained("IDEA-Research/grounding-dino-tiny")
AutoModelForZeroShotObjectDetection.from_pretrained("IDEA-Research/grounding-dino-tiny")
print(">> SAM 2 (hiera-large)")
SAM2ImagePredictor.from_pretrained("facebook/sam2-hiera-large")
print(">> weights cached")
PY
fi

echo ""
echo ">> done. activate with:  source tools/segmentation/.venv/bin/activate"
echo ">> Stage 1:  python segment.py --image PATH --prompts 'audio recorder' 'dried flower'"
echo ">> Stage 2:  python render.py --apply 'audio recorder:floyd_steinberg' --apply 'dried flower:posterize:levels=4'"
