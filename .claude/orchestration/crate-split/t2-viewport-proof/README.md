# Viewport session proof ownership

The post-extraction workspace census found that this scene proof uses both
scene and water APIs. Move it unchanged to the catalog's GPU proof binary,
which owns cross-family contracts. Both binaries are named gpu_proofs; the
module and test names stay unchanged.

Commit this plan before the file move. Update the two integration module
mounts and recorded timing ownership separately, then repeat the failed census.
