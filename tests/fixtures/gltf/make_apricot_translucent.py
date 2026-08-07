#!/usr/bin/env python3
"""Add KHR_materials_diffuse_transmission (factor 0.5) to every material in a GLB.

GLB layout: 12-byte header (magic, version, total length), then chunks
(chunk length u32, chunk type u32, data). Chunk 0 is JSON (type 0x4E4F534A),
4-byte aligned, space-padded.
"""
import json
import struct
import sys

src, dst, factor = sys.argv[1], sys.argv[2], float(sys.argv[3])
data = bytearray(open(src, "rb").read())
magic, version, _total = struct.unpack_from("<III", data, 0)
assert magic == 0x46546C67, "not a GLB"
jlen, jtype = struct.unpack_from("<II", data, 12)
assert jtype == 0x4E4F534A, "first chunk not JSON"
gltf = json.loads(data[20 : 20 + jlen])

n = 0
for mat in gltf.get("materials", []):
    ext = mat.setdefault("extensions", {})
    ext["KHR_materials_diffuse_transmission"] = {"diffuseTransmissionFactor": factor}
    n += 1
used = gltf.setdefault("extensionsUsed", [])
if "KHR_materials_diffuse_transmission" not in used:
    used.append("KHR_materials_diffuse_transmission")

payload = json.dumps(gltf, separators=(",", ":")).encode()
pad = (4 - len(payload) % 4) % 4
payload += b" " * pad
rest = bytes(data[20 + jlen :])  # remaining chunks (BIN), unchanged
out = bytearray()
out += struct.pack("<II", magic, version)
out += struct.pack("<I", 12 + 8 + len(payload) + len(rest))
out += struct.pack("<II", len(payload), 0x4E4F534A)
out += payload
out += rest
open(dst, "wb").write(out)
print(f"{n} materials -> factor {factor}; {src} -> {dst}")
