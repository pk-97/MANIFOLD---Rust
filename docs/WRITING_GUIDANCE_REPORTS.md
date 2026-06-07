# Writing Preset Guidance Reports: A Working Guide

This is the "how to think and how to do" guide for authoring an illustrated guidance report
for a preset (an effect or generator). A guidance report is a markdown walkthrough that pairs a
plain-language explanation of each node group with a live image of what that group actually
produces, so an intelligent non-specialist can read straight from the graph's nodes to the
picture on screen.

The reference implementation is **Oily Fluid**. Before authoring a new one, read both:

- the authored source: `crates/manifold-renderer/assets/generator-presets/OilyFluid.guidance.md`
- a generated result: run the tool (below) and open `report/oily_fluid/report.md`.

This guide exists because the Oily Fluid report took many rounds of iteration to get right.
Everything that iteration taught is encoded here as rules. Follow them and a new report should
land in one pass, not ten.

---

## 1. The two pieces

A report is produced by a **tool** from an **authored sidecar**. You write the sidecar. The tool
renders the images and assembles the final markdown.

- **Tool:** `crates/manifold-renderer/src/bin/preview_report.rs` (`preview-report` bin). It loads a
  preset headless, warms the simulation, captures each group's output through the editor's smart
  preview, and writes PNGs plus `report.md`.
- **Sidecar:** `<PresetStem>.guidance.md` next to the preset JSON. This is the authored document.
  The tool injects the rendered images into it at tokens. If no sidecar exists, the tool falls
  back to an auto-generated per-group report, which is a draft scaffold, not a finished report.

You never touch the rendering code. The smart-preview encodings live in one shared place
(`crates/manifold-renderer/src/node_graph/preview_encode.rs`) used by both the editor and this
tool, so an image in a report is exactly what the editor shows.

---

## 2. Running the tool

```
cargo run -p manifold-renderer --bin preview-report -- <preset> [--frames N] [--res W] [--nodes] [--params <file>]
```

- `<preset>`: a bundled name (`OilyFluid`) or a path to a preset `.json`. A graph pulled out of a
  `.manifold` project also works (see §8).
- `--frames N`: warm-up frames rendered before any capture. Feedback simulations start black and
  develop over time, so this matters. Default 240. For a developed look use 500 or more.
- `--res W`: square capture resolution. Default 512.
- `--nodes`: also capture every top-level node, not just groups. Rarely needed for a report.
- `--params <file>`: a JSON object `{ "paramId": value, ... }` of outer-card values. Without it,
  every parameter sits on its default, which usually looks boring. See §8.

Output lands in `report/<slug>/` relative to the working directory: one PNG per captured group
plus `report.md`. This output is generated, not committed.

---

## 3. The sidecar and its tokens

The sidecar is normal markdown with a few tokens the tool resolves. Use tokens. Never hardcode an
image filename, because filenames are positional (`04_inject_noise.png`) and shift when groups are
renamed or reordered, and because the tool also picks the encoding and appends the caption.

- `![alt text](preview://<Group Handle>)`: the group's output image. A group resolves to the node
  that produces its output port. Example: `![Curl Forcing](preview://Curl Forcing)`.
- `![alt text](preview://node:<nodeId>)`: one specific inner node's output, addressed by its
  stable `nodeId`. Use this when a group's output port is not the illustrative thing (see §7).
- `![alt text](preview://Output)`: the whole preset's final output, tonemapped to match the live
  screen.
- `[[preview-legend]]`: expands to the "Reading the images" key, covering only the encodings that
  actually appear in this report.

The tool appends a one-line caption under each image naming its encoding, for example
`*Flow wheel.*`. You may add a further italic sentence of context as a separate paragraph right
after the image (the seed and output images in Oily Fluid do this). Keep that extra context to one
sentence.

---

## 4. Audience

Write for an **intelligent non-specialist**: a TouchDesigner, Resolume, or Ableton-grade creative.
Assume fluency with layers, blending, blur, and parameters. Do not assume any graphics or
simulation background.

That means these terms are unknown and must be handled (define on first use, or replace):
gradient, advection, curl, normal map, attenuate, convolve, divergence, vorticity.

And these are jargon to avoid entirely in the narrative, replaced by the concrete named thing:

- **"field"**: say "the flow" or "the color", the thing the report already named. The word "field"
  on its own means nothing to this reader.
- **"buffer"**: say "the color", "the flow", or "the two things kept in memory".

---

## 5. Hard rules (invariants)

These are not style preferences. Breaking one is a defect.

1. **No em-dashes, no semicolons.** Anywhere. Use periods, commas, colons, and parentheses. This is
   the project copy voice and applies to every word a user reads, including the tool-generated
   legend strings.
2. **Define every load-bearing term at first use, in plain language.** A term is load-bearing if a
   reader must understand it to follow the narrative. In Oily Fluid that meant defining gradient,
   advection, curl, and normal map inline the first time each appears. Pure technique names that
   the narrative does not depend on go to the Technical notes (§7), not inline.
3. **Every phase opens with a Reads/Produces line.** An italic line stating exactly what the phase
   reads and what it produces. This is the single most important clarity device. It makes the data
   flow explicit instead of leaving the reader to infer it.
4. **Tokens, never filenames.** Images are `preview://...` tokens, the legend is `[[preview-legend]]`.
   A report with hardcoded paths is a dead snapshot that rots on the next regenerate.
5. **For a feedback simulation, narrate chronologically from the first frame.** Start from black,
   where the buffers are empty, and build up. Never describe the steady-state loop as if it were
   already running, because then phrases like "reads the previous frame" have no origin and the
   reader is lost. Give the loop a beginning.
6. **Show the meaningful signal, not just the group's output port.** If a group's output is a
   degenerate or tiny signal (a noise pattern after a near-zero injection gain reads as black),
   point the image at the inner node that carries the real structure with `preview://node:<id>`,
   and say in a caption why.
7. **Verify before declaring done.** Run the tool and confirm zero unmatched tokens, zero
   em-dashes, zero semicolons, zero bare "field", and that no image is uniformly black. See §10.

---

## 6. Tone and language

- **Professional and precise, but readable.** The target is a good textbook or a well-written
  manual. Name operations correctly and state their consequence concretely. "Computes the gradient
  of the color, then rotates it 90 degrees" is right. "Gives it a sideways push" is hand-waving and
  is not allowed.
- **No casual or cute language.** Not "whisper", "smear", "lays down", "rolls together". These read
  as imprecise and unprofessional.
- **No vague verbs.** "Pushes", "turns into", "handles" hide the actual operation. Say what happens.
- **Explain the why, concretely.** After naming an operation, say what it does to the result and
  why that matters for the look. The reader should come away understanding cause and effect, not
  just a list of steps.
- **Introduce a hard idea by its simplest case first.** Advection is defined at Advect Color (the
  picture visibly dragged along the flow), the intuitive case, and only then referenced back to
  Advect Velocity (the flow moving itself), the abstract case. Do not lead with the abstract case.

---

## 7. The canonical structure

This is the section order the Oily Fluid report converged on. Follow it for a feedback simulation.
Adapt it for other shapes as noted at the end.

1. **Title and tagline.** `# Name` then a one-line italic tagline naming what it is and the look it
   produces.
2. **## How it works.** One or two sentences: it is a generator or effect, what it takes as input
   (a generator takes none and starts from black), and that it builds the image over time.
3. **The node-groups framing.** One sentence stating the report walks the named node groups in the
   order they run. This ties the document to the actual graph.
4. **### The two things that persist** (feedback simulations). Name the state carried between
   frames, each defined concretely. For Oily Fluid that is the color and the flow. Make clear
   everything else is rebuilt each frame.
5. **### The loop in one breath.** A numbered list of the phases, each with its input named, and an
   explicit statement of where the loop closes. This primes the reader before any detail.
6. **### Reading the images.** Just the `[[preview-legend]]` token.
7. **### Phase N: Name.** Each phase section opens with the italic *Reads: ... Produces: ...* line,
   then plain prose explaining the work, then the image token(s). Multi-step phases use bold
   run-in names for each step.
8. **### Putting it together: the first few frames.** The chronological build from black, frame 1,
   frame 2, frame 3 onward. This is where the accumulation and the feedback close are made concrete.
   End with the `preview://Output` image.
9. **## Controls.** One or two tables (Simulation, Look) built from the preset's parameters, each
   row saying what the control does in plain language.
10. **## Technical notes.** The specifics for readers who want them: the real algorithm names and
    one-line technique descriptions (simplex noise, semi-Lagrangian advection, LIC, Lambertian,
    Fresnel, Blinn, matcap). Heavy rendering and simulation vocabulary lives here, out of the
    narrative, so it stops interrupting the plain-language flow.

**Adapting for non-feedback presets.** A linear effect chain has no persistent state and no
frame-by-frame buildup. Drop sections 4 and 8. Keep the node-groups framing, the loop-in-one-breath
(reframed as a straight pipeline of stages with their inputs), the per-section Reads/Produces lines,
the legend, the controls, and the technical notes. The Reads/Produces invariant still carries the
whole thing.

---

## 8. Images, encodings, and parameters

- **Encodings are automatic.** The tool picks the smart-preview encoding per node (raw color,
  brightness lift for low-amplitude data, the flow wheel for velocity and force, signed, normal,
  depth) and lifts dark color intermediates so they are legible. The hero `Output` is tonemapped to
  match the live screen. You do not choose encodings, but you should understand them well enough to
  caption and explain them. The legend covers what each means.
- **Warm-up matters.** A feedback simulation is black at frame 0 and develops over time. Too few
  frames gives black or flat images. Use `--frames 500` or more for a developed look, and explain
  the warm-up in the report (Oily Fluid notes that the flow settles in a second or two while the
  color builds over several seconds).
- **Default parameters usually look boring.** They are conservative. For an interesting report, use
  real settings. The fastest source is an existing `.manifold` project where the preset was tuned:
  extract the layer's `generatorGraph` as the preset JSON and its `genParams.paramValues` as the
  `--params` map. For a `.manifold` (a ZIP), the project lives at `project.json` inside, and a
  layer's generator is at `timeline.layers[i].generatorGraph` with values at
  `timeline.layers[i].genParams.paramValues`.
- **Some groups have no image, and that is fine.** A group whose output is a particle array or a
  scalar (a control box, a spawn step, a particle move step) produces no texture, and the tool skips
  it with a "no image output" message. Do not put a `preview://` token for such a group, or it will
  show as an unmatched token. Describe these groups in text with their Reads and Produces, and where
  their effect is visible through another group's image, say so. Fluid Sim 2D does this for Spawn
  Particles, Move Particles, Resolution Scaling, and Clip Triggers: the particles have no picture of
  their own, you see them through Render Density.
- **When a group output is not illustrative, show an inner node.** Oily Fluid's Inject Noise group
  outputs the noise after a tiny injection gain, which reads as black. The report instead shows the
  pre-gain pattern with `preview://node:noise_combine` and a caption explaining that it is scaled
  down hard before use. Trace the group's wires, find the node that carries the structure a reader
  needs to see, and point the image there.

---

## 9. The authoring procedure

1. **Read the graph end to end.** Identify the groups, what each one produces, and for a feedback
   preset, the edges that carry state between frames. Do not write about a group you have not traced.
2. **Order the phases.** Usually the phases are the top-level groups in the order they run. Confirm
   the order by following the wires, not by guessing from names.
3. **Determine Reads and Produces for each group.** Trace each group's input and output wires.
   These become the Reads/Produces lines, and getting them right is what makes the report correct.
4. **Get good parameters and a warm-up count.** Pull tuned values from a project or set sensible
   ones, and pick a frame count that develops the image.
5. **Write the sidecar** using the §7 structure and the §3 tokens. Define terms as you go (§4, §5).
6. **Run the tool and verify** (§10).
7. **Read the result as the audience** and tighten the language against §5 and §6. Any term you
   would have to look up is a term to define or replace.

---

## 10. Verification checklist

Run the tool, then check the generated `report.md`:

- The run log prints `using authored guidance: ...` and `wrote ...`. If it did not use the guidance,
  the sidecar is misnamed or in the wrong place.
- **Zero unmatched tokens.** Search the output for `preview://` and `[[preview`. Any hit means a
  token did not resolve, usually a group-handle typo. The tool also prints a warning for the first
  unmatched token.
- **Zero em-dashes and semicolons.** Search for both.
- **Zero bare "field" and "buffer"** in the narrative (the mode names Flow Field and Height Map are
  proper nouns and are fine).
- **No uniformly black or solid-color image.** A tiny PNG file size is the tell. If one is dead,
  either warm up longer, use better parameters, or point that image at a more meaningful node (§8).
- **Every load-bearing term is defined at first use.** Read it as someone who does not know
  graphics. The first time you hit a term that is not explained, fix it.

---

## 11. Anti-patterns (the mistakes this guide exists to prevent)

- Describing the feedback loop in steady state with no starting point, so "the previous frame" hangs
  in the air.
- Using a term as a bare verb or noun without defining it: advect, gradient, normal map, curl,
  attenuate, convolve, field, buffer.
- Hardcoding image filenames instead of `preview://` tokens, producing a snapshot that rots.
- Em-dashes and semicolons.
- Casual language (whisper, smear, lay down) or vague verbs (push, turn into).
- Capturing a group's output port when it is a degenerate signal, so the image reads as black.
- Using default parameters on a slow feedback simulation, so the output looks flat and boring.
- Cramming the rendering math (LIC, Lambertian, Fresnel, Blinn) into the narrative instead of the
  Technical notes.

---

## 12. Reference

- **Authored sidecar:** `crates/manifold-renderer/assets/generator-presets/OilyFluid.guidance.md`
- **Tool:** `crates/manifold-renderer/src/bin/preview_report.rs`
- **Shared smart-preview encodings:** `crates/manifold-renderer/src/node_graph/preview_encode.rs`
  (one source of truth with the editor, do not duplicate)
- **Encoding meanings:** the `encoding_legend` function in the tool, surfaced in every report by the
  `[[preview-legend]]` token.

Read the Oily Fluid sidecar end to end before writing a new report. It is the worked example this
guide describes.
