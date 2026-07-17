# SPDX-License-Identifier: GPL-2.0-or-later
#
# GPL for the same reason as fbx2glb.py (imports bpy); invoked only as a
# subprocess, never linked.
#
# Generator for `tests/fixtures/gltf/hostile/mixamo_like.glb`'s source FBX:
# a Mixamo-shaped hostile asset — skinned mesh, 2-bone keyframed armature,
# the whole hierarchy under an ancestor with a 0.01 unit-conversion scale
# (the exact trait class of BUG-205: transform-bearing ancestors ABOVE the
# joint tree, which Khronos gate fixtures like CesiumMan/Fox never have).
# The checked-in fixture is the converted glb, so the test suite never
# needs Blender; regenerate with:
#
#   /Applications/Blender.app/Contents/MacOS/Blender -b -P scripts/blender/make_hostile_rig.py -- /tmp/mixamo_like.fbx
#   /Applications/Blender.app/Contents/MacOS/Blender -b -P scripts/blender/fbx2glb.py -- /tmp/mixamo_like.fbx tests/fixtures/gltf/hostile/mixamo_like.glb
import sys

import bpy

out = sys.argv[sys.argv.index("--") + 1]

bpy.ops.wm.read_factory_settings(use_empty=True)

arm = bpy.data.armatures.new("Rig")
arm_obj = bpy.data.objects.new("Armature", arm)
bpy.context.collection.objects.link(arm_obj)
bpy.context.view_layer.objects.active = arm_obj
bpy.ops.object.mode_set(mode="EDIT")
b0 = arm.edit_bones.new("lower")
b0.head = (0, 0, 0)
b0.tail = (0, 0, 50)
b1 = arm.edit_bones.new("upper")
b1.head = (0, 0, 50)
b1.tail = (0, 0, 100)
b1.parent = b0
bpy.ops.object.mode_set(mode="OBJECT")

bpy.ops.mesh.primitive_cylinder_add(radius=10, depth=100, location=(0, 0, 50))
mesh_obj = bpy.context.active_object
mesh_obj.name = "Body"
vg0 = mesh_obj.vertex_groups.new(name="lower")
vg1 = mesh_obj.vertex_groups.new(name="upper")
for v in mesh_obj.data.vertices:
    w = min(max(v.co.z / 100.0 + 0.5, 0.0), 1.0)
    vg0.add([v.index], 1.0 - w, "REPLACE")
    vg1.add([v.index], w, "REPLACE")
# A real material — materialless geometry lands in glTF's default-material
# bucket, which the importer reports as a separate (known) gap; this
# fixture targets the transform class, not that one.
mat = bpy.data.materials.new("Bone")
mat.use_nodes = True
bsdf = mat.node_tree.nodes["Principled BSDF"]
bsdf.inputs["Base Color"].default_value = (0.8, 0.7, 0.5, 1.0)
mesh_obj.data.materials.append(mat)
mod = mesh_obj.modifiers.new("Armature", "ARMATURE")
mod.object = arm_obj
mesh_obj.parent = arm_obj

bpy.context.view_layer.objects.active = arm_obj
bpy.ops.object.mode_set(mode="POSE")
pb = arm_obj.pose.bones["upper"]
pb.rotation_mode = "XYZ"
for frame, rx in [(1, 0.0), (12, 1.0), (24, 0.0)]:
    pb.rotation_euler = (rx, 0, 0)
    pb.keyframe_insert("rotation_euler", frame=frame)
bpy.ops.object.mode_set(mode="OBJECT")

root = bpy.data.objects.new("UnitConversion", None)
bpy.context.collection.objects.link(root)
root.scale = (0.01, 0.01, 0.01)
arm_obj.parent = root

bpy.ops.export_scene.fbx(filepath=out, add_leaf_bones=True, bake_anim=True)
print("wrote", out)
