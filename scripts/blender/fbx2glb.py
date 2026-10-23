# SPDX-License-Identifier: GPL-2.0-or-later
#
# This script imports `bpy` and is therefore an extension of Blender under
# the Blender Foundation's licensing convention — it is GPL, deliberately
# separate from the MANIFOLD Rust codebase's license. MANIFOLD only ever
# invokes it as a subprocess (`Blender -b -P ...`), which keeps the GPL on
# this side of the process boundary.
#
# FBX (or OBJ/DAE — anything Blender imports) in, glb out:
#
#   /Applications/Blender.app/Contents/MacOS/Blender -b -P scripts/blender/fbx2glb.py -- input.fbx output.glb
#
# This is the conversion leg for Mixamo rigs and other FBX-only assets —
# MANIFOLD imports glTF only, by design (FBX is a closed format; the open
# pipeline converts). Proven against Blender 4.5.2 LTS, 2026-07-17.
import sys

import bpy

argv = sys.argv[sys.argv.index("--") + 1 :]
src, dst = argv[0], argv[1]

bpy.ops.wm.read_factory_settings(use_empty=True)
lower = src.lower()
# Models keep their true size in metres. FBX and COLLADA state their units and
# are converted to metres; OBJ has none and is read as metres. glTF out is
# metres.
if lower.endswith(".fbx"):
    # The importer always applies the file's UnitScaleFactor.
    bpy.ops.import_scene.fbx(filepath=src, global_scale=1.0)
elif lower.endswith(".obj"):
    bpy.ops.wm.obj_import(filepath=src, global_scale=1.0)
elif lower.endswith(".dae"):
    bpy.ops.wm.collada_import(filepath=src, import_units=True)
else:
    raise SystemExit(f"unsupported input extension: {src}")
# COLLADA's units land on the scene's unit scale, which glTF export ignores:
# fold it into the top-level objects so the file comes out in metres.
metres_per_unit = bpy.context.scene.unit_settings.scale_length
if metres_per_unit != 1.0:
    for obj in bpy.context.scene.objects:
        if obj.parent is None:
            obj.location *= metres_per_unit
            obj.scale *= metres_per_unit
    bpy.context.scene.unit_settings.scale_length = 1.0
bpy.ops.export_scene.gltf(filepath=dst, export_format="GLB", export_animations=True)
print("wrote", dst)
