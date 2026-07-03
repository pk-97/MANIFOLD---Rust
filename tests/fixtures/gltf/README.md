# glTF import fixtures (IMPORT_DESIGN P1)

Two tiers — see `docs/IMPORT_DESIGN.md` §8 addendum:

1. **Khronos glTF sample models** — conformance fixtures, fetched per the P1 brief.
2. **Canonical real-world fixture:** the CC0 Stewartia monadelpha photoscan —
   multi-material, alpha-masked foliage, photoscan-scale vertex counts.
   https://sketchfab.com/3d-models/cc0-himesyara-stewartia-monadelpha-cae7436738674d3586930c206f51073b
   Sketchfab downloads require a login: **Peter downloads this by hand** (glTF
   format) into this directory before P1 starts. CC0 = committable.

P1's gate renders the stewartia headless to PNG and compares against Sketchfab's
own preview by eye — a look, not just a green test.
