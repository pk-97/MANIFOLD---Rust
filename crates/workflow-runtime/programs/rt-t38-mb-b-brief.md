You are a mechanical code-change generator. Read the brief and the current file, then output ONLY a JSON object, no prose, no markdown fences:

{"edits": [{"path": "<repo-relative path>", "find": "<exact text currently in the file>", "replace": "<replacement text>"}, ...],
 "writes": [],
 "commit_message": "<one line>"}

Rules:
- `find` must be copied EXACTLY from the current file below (whitespace included) and must be unique in the whole file — include enough surrounding lines to be unique.
- Make exactly the one edit this brief demands, nothing else.

# Brief: MB-B — second GI bounce (RAYTRACING_DESIGN.md section 11.4)

One edit in `crates/manifold-gpu/src/metal/raytrace.rs`, inside the `SHADOW_RAYS_MSL`
string: the GI gather's depth constant goes from 1 to 2. MB-A already built the bounce
loop, the throughput carry, and the extension sampling — they are live code the moment
this constant rises.

Change:

    const uint RT_GI_MAX_BOUNCES = 1u;

to:

    const uint RT_GI_MAX_BOUNCES = 2u;

and update the adjacent comment's "MB-A ships the loop at depth 1 (byte-identical to the
pre-loop gather); MB-B raises the depth to 2." to "MB-B: depth 2 — one extension bounce
carrying intermediate albedo (colour bleed)." — keep the rest of the comment block
(including the range note) untouched.

commit_message: "MB-B: RT_GI_MAX_BOUNCES 1 -> 2 — second GI bounce live (RAYTRACING_DESIGN.md section 11 MB4)"

# Current file

{{file:crates/manifold-gpu/src/metal/raytrace.rs}}
