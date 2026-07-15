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

3. **`DamagedHelmet.glb`** (IMPORT_FIDELITY_DESIGN.md F-P4 held-out fixture) —
   Khronos Sample Models "Damaged Helmet" (glTF-Sample-Assets), CC-BY 4.0,
   https://github.com/KhronosGroup/glTF-Sample-Assets/tree/main/Models/DamagedHelmet
   (model by theblueturtle_, © 2016, licensed CC-BY 4.0). Chosen because it
   carries all five glTF PBR map types (base colour, normal, metallic-
   roughness, occlusion, emissive) — exactly F-P4's per-map colour-space and
   port-wiring gate. Tracked (not gitignored): CC-BY permits redistribution
   with attribution, satisfied by this line.
