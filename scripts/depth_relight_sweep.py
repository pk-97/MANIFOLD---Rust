#!/usr/bin/env python3
"""Append the canonical 2.5D relight tail to any preset JSON.

The tail: luminance -> blurred height -> normals -> Lambert + masked
specular + masked Fresnel rim -> GTAO (4x8) denoised by two bilateral
passes -> multiply -> tone map -> final_output. All stock atoms.
"""
import json, sys, copy

import os
BASE = (sys.argv[1].rstrip('/') + '/') if len(sys.argv) > 1 else os.environ.get('SWEEP_OUT', '/tmp/depth-relight-sweep/')
os.makedirs(BASE, exist_ok=True)

def F(v): return {"type": "Float", "value": float(v)}
def E(v): return {"type": "Enum", "value": int(v)}

def append_tail(doc, z_scale=3.0, ao_radius=0.012, ao_intensity=1.1, ambient=0.30):
    doc = copy.deepcopy(doc)
    nodes, wires = doc['nodes'], doc['wires']
    fo = next(n['id'] for n in nodes if n['typeId'] == 'system.final_output')
    into_fo = [w for w in wires if w['toNode'] == fo]
    assert into_fo, 'no wire into final_output'
    src = into_fo[0]['fromNode']
    src_port = into_fo[0]['fromPort']
    doc['wires'] = [w for w in wires if w['toNode'] != fo]
    nid = max(n['id'] for n in nodes) + 1
    ids = {}
    def add(key, typeId, params=None, title=None):
        nonlocal nid
        ids[key] = nid
        n = {"id": nid, "nodeId": f"rl_{key}", "typeId": typeId,
             "handle": f"rl_{key}", "title": title or f"RL {key}"}
        if params: n["params"] = params
        nodes.append(n); nid += 1
    def wire(a, ap, b, bp):
        doc['wires'].append({"fromNode": a, "fromPort": ap, "toNode": b, "toPort": bp})

    add('gray', 'node.saturation', {"saturation": F(0)}, 'Height Field')
    # Normals come from the UNBLURRED height — lighting detail must match the
    # source's spatial frequency (the blurred-normals version read "blurry
    # and off"). Only the AO depth input gets a small blur to calm speckle.
    # Anti-banding: fp16 height differentiated at 1 texel quantizes smooth
    # gradients into stepped normals. Cure = ordered dither (breaks the
    # quantization correlation) + a tiny 9-tap sigma~2 blur (interpolates
    # across steps) BEFORE the derivative. This is the small blur, not the
    # 17-tap default that caused the earlier haze.
    add('dnoise', 'node.noise', {"type": E(2), "scale": F(997.0)}, 'Dither Noise')
    add('dscale', 'node.scale_offset_image', {"scale": F(0.003), "offset": F(-0.0015)}, 'Dither Amp')
    add('dither', 'node.mix', {"amount": F(1), "mode": E(2)}, 'Height Dither')
    add('nbx', 'node.gaussian_blur', {"axis": E(0), "kernel_size": E(0)}, 'Normal Blur X')
    add('nby', 'node.gaussian_blur', {"axis": E(1), "kernel_size": E(0)}, 'Normal Blur Y')
    add('bumps', 'node.surface_bumps', {"z_scale": F(z_scale)}, 'Height To Normal')
    add('lambert', 'node.basic_light', {"ambient": F(ambient)}, 'Lambert')
    # Heightfield-native GTAO (projection=Height Field): reads the dithered
    # height directly in an ortho frame, all math fp32 in-kernel — no depth
    # window, no synthetic-camera numbers, no levels hack. The camera wire
    # satisfies the required port but is unused in this mode.
    add('cam', 'node.look_at_camera', None, 'Camera (unused)')
    add('ao', 'node.ssao_gtao', {"radius": F(0.02), "intensity": F(1.3),
                                 "slices": F(4), "steps": F(8),
                                 "projection": E(1), "relief": F(0.25)}, 'GTAO Heightfield')
    add('abx', 'node.gaussian_blur', {"axis": E(0), "kernel_size": E(0)}, 'AO Soften X')
    add('aby', 'node.gaussian_blur', {"axis": E(1), "kernel_size": E(0)}, 'AO Soften Y')
    # AO darkens the SHADING term, never the source color — keeps saturation.
    add('shade', 'node.mix', {"amount": F(1), "mode": E(4)}, 'Lambert x AO')
    add('lit', 'node.mix', {"amount": F(1), "mode": E(4)}, 'Source x Shading')
    add('spec', 'node.shininess', {"power": F(48)}, 'Specular')
    add('specmask', 'node.mix', {"amount": F(1), "mode": E(4)}, 'Spec Mask')
    add('plusspec', 'node.mix', {"amount": F(1), "mode": E(2)}, 'Add Spec')
    add('boost', 'node.exposure', {"gain": F(1.4)}, 'Relight Gain')

    wire(src, src_port, ids['gray'], 'in')
    wire(ids['dnoise'], 'out', ids['dscale'], 'in')
    wire(ids['gray'], 'out', ids['dither'], 'a')
    wire(ids['dscale'], 'out', ids['dither'], 'b')
    wire(ids['dither'], 'out', ids['nbx'], 'in')
    wire(ids['nbx'], 'out', ids['nby'], 'in')
    wire(ids['nby'], 'out', ids['bumps'], 'in')
    wire(ids['bumps'], 'out', ids['lambert'], 'normal')
    wire(ids['bumps'], 'out', ids['spec'], 'normal')
    wire(ids['dither'], 'out', ids['ao'], 'depth')
    wire(ids['cam'], 'out', ids['ao'], 'camera')
    wire(ids['ao'], 'out', ids['abx'], 'in')
    wire(ids['abx'], 'out', ids['aby'], 'in')
    wire(ids['lambert'], 'out', ids['shade'], 'a')
    wire(ids['aby'], 'out', ids['shade'], 'b')
    wire(src, src_port, ids['lit'], 'a')
    wire(ids['shade'], 'out', ids['lit'], 'b')
    wire(ids['spec'], 'out', ids['specmask'], 'a')
    wire(src, src_port, ids['specmask'], 'b')
    wire(ids['lit'], 'out', ids['plusspec'], 'a')
    wire(ids['specmask'], 'out', ids['plusspec'], 'b')
    wire(ids['plusspec'], 'out', ids['boost'], 'in')
    wire(ids['boost'], 'out', fo, 'in')
    return doc

def minimal_generator(name, body_nodes, body_wires, out_node):
    return {
        "version": 2, "name": name, "description": name,
        "presetMetadata": {"id": name, "displayName": name, "category": "Pattern",
                           "oscPrefix": name[0].lower()+name[1:], "available": False,
                           "isLineBased": False, "params": [], "bindings": [],
                           "skipMode": {"kind": "never"}},
        "nodes": ([{"id": 0, "nodeId": "inputs", "typeId": "system.generator_input",
                    "handle": "inputs", "title": "Inputs"}] + body_nodes +
                  [{"id": 99, "nodeId": "final_output", "typeId": "system.final_output",
                    "handle": "final_output", "title": "Output"}]),
        "wires": body_wires + [{"fromNode": out_node, "fromPort": "out", "toNode": 99, "toPort": "in"}],
    }

REPO = '/Users/peterkiemann/MANIFOLD - Rust'
variants = {}

# Fronts: three procedural + two shipped presets, untouched, tail appended.
# (The probe session's feedback-vortex front lived in session scratchpad; the
# committed sweep uses the reproducible fronts below.)
# B: noise terrain — signed simplex remapped to [0,1].
terrain = minimal_generator('SweepTerrain', [
    {"id": 1, "nodeId": "noise", "typeId": "node.simplex_field_2d", "handle": "noise",
     "params": {"scale_x": F(3.0), "scale_y": F(3.0)}, "title": "Noise"},
    {"id": 2, "nodeId": "remap", "typeId": "node.scale_offset_image", "handle": "remap",
     "params": {"scale": F(0.5), "offset": F(0.5)}, "title": "To 0..1"},
], [
    {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in"},
], 2)
variants['SweepTerrain'] = (append_tail(terrain, z_scale=4.0), 'generator')

# C: voronoi cells.
voro = minimal_generator('SweepVoronoi', [
    {"id": 1, "nodeId": "cells", "typeId": "node.voronoi_2d", "handle": "cells",
     "params": {"scale": F(6.0)}, "title": "Voronoi"},
], [], 1)
variants['SweepVoronoi'] = (append_tail(voro, z_scale=4.0), 'generator')

# D: shipped Caustics generator, untouched, tail appended.
caustics = json.load(open(REPO + '/crates/manifold-renderer/assets/generator-presets/Caustics.json'))
variants['SweepCaustics'] = (append_tail(caustics), 'generator')

# E: shipped Watercolor effect (UV-gradient source in the harness), tail appended.
water = json.load(open(REPO + '/crates/manifold-renderer/assets/effect-presets/Watercolor.json'))
variants['SweepWatercolor'] = (append_tail(water), 'effect')

for name, (doc, kind) in variants.items():
    doc['presetMetadata']['id'] = name
    doc['name'] = name
    path = BASE + name + '.json'
    json.dump(doc, open(path, 'w'), indent=1)
    print(kind, path)
