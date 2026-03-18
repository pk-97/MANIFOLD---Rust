# Depth Models

Drop your ONNX monocular depth model in this folder.

`DepthEstimatorPlugin.cpp` will auto-try these filenames (in order):

1. `midas_small_256.onnx`
2. `midas_small.onnx`
3. `depth_anything_v2_vits.onnx`

You can also override model selection via env var:

`MANIFOLD_DEPTH_MODEL=/absolute/path/to/model.onnx`

## Subject Segmentation Models (Optional)

For subject-only wireframe isolation, drop a lightweight ONNX segmentation model here.

Auto-try filenames:

1. `subject_segmentation_256.onnx`
2. `selfie_segmentation_256.onnx`
3. `human_segmentation_256.onnx`
4. `person_segment_lite.onnx`

Or override via:

`MANIFOLD_SUBJECT_MODEL=/absolute/path/to/model.onnx`

Expected output:

- Foreground probability mask (single channel), or
- 2-channel bg/fg logits/probabilities.
