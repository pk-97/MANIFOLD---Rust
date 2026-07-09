# Mapping Cards — the taste corpus

**Status:** DRAFT CORPUS authored 2026-07-10 (Fable + Peter's brief: "think like a human — what is musically interesting"). **Every card is `draft-unjudged`**: structure follows [MAPPING_GRAMMAR_DESIGN.md](MAPPING_GRAMMAR_DESIGN.md); the taste is Fable's first pass and becomes Peter's only after he judges it against real music in the app. A card loses the flag when judged; auto-populate treats judged cards as authoritative and drafts as provisional.
**Scope:** all shipped generator presets (19) + shipped effect presets (24) + the [VISUAL_PIECES_GRAPH_DRAFTS.md](VISUAL_PIECES_GRAPH_DRAFTS.md) pieces.

## House rules

Systematic taste lives HERE, once — a veto on any rule edits one line and propagates to every card. Cards only state what's specific to the piece.

- **H1 — Events hit, envelopes breathe.** An event (kick, transient) may drive a continuous param only as an *impulse*: near-instant attack, musical decay. Event → symmetric swell is the screensaver anti-pattern and never legal.
- **H2 — Tier match.** Audio structure drives visual change of the same weight: texture↔grain/shimmer · beat↔pulse/step/burst · bar↔motion/intensity · phrase↔camera/reveal/regime · section↔palette/scene/mode.
- **H3 — Kick owns exactly one param per preset.** The kick move is the preset's signature. A second kick target is a deliberate, drop-only exception.
- **H4 — Hue and palette never move on beat-tier events.** Palette steps at section boundaries (phrase at fastest).
- **H5 — Character params stay hands-only.** Flock character, camera FOV, text content/size, line weights: the params that define *what the piece is* are set by hand per track and never auto-rolled.
- **H6 — Builds build.** Where a piece has a natural anticipation move (converge, compress, fold, brighten, approach), its card names it; it rides the riser and resolves at the drop (snap or release — stated per card).
- **H7 — Max three mapped voices sounding at once**, counted across the whole layer stack (generator + its effects): one event voice, one energy voice, one structure voice. Denser = drop-only exception.
- **H8 — Camera moves are phrase-tier or slower.** Never beat.
- **H9 — Strobe-class moves (strobe, invert-flash, hard glitch) engage in builds/drops only**, never quiet sections; their rates are beat-quantized.
- **H10 — Default energy smoothing is asymmetric**: fast up, slow down (beat-adjacent ≈ a15/r800; bar ≈ a250/r1500; phrase ≈ 1–2s both). Symmetric lag reads underwater.
- **H11 — Roll-time vs live.** Some card entries are configuration at roll time (BPM-locked rates, counts, per-section palettes), not live wires — marked `roll`.
- **H12 — Unshipped detectors.** Rows on future features are marked † and ignored by auto-populate until the detector lands. `~` marks in-flight/derivable signals.
- **H13 — Engage = clip placement.** "Engage: drop" means the binding lives on the clips rolled into drop sections — no runtime conditional needed. (Structural assumption, Peter-unconfirmed: bindings ride the clip, rolls place clips per section.)

## Vocabulary

Row format: `Param — tier · feature · mode · envelope · engage`. Omitted envelope = H10 default for the tier; omitted engage = always.
**Features:** `kick` `trans` (transient) — events, shipped · `Low/Mid/High/energy` — band-send envelopes (user-configured bands), shipped · `riser~` — sweep/riser event (in flight) · `dens~` — onset density · `beatN` — beat_ramp over N beats · `lfoN` — beat-synced LFO, N-beat period · `pad` — clip/MIDI trigger, performer-owned · `§step†` — section-boundary step · `§class†` — section class · `pitch†` — pitch/chroma.
**Modes:** `cont` (continuous) · `ride` (follows a ramp, resolution stated) · `step` · `random` · `impulse` (H1) · `trigger`.
**Env:** `aN/dN` ms attack/decay for impulses; `aN/rN` for continuous.
**Hands:** deliberately unmapped (H5 or restraint) — auto-populate must not roll these.

---

# Shipped generators

### Tesseract (Geometry) — draft-unjudged
*Signature: **Dimension rides the build** — square→cube→tesseract as the riser climbs; full 4D lands exactly on the drop. The dimension-morph is the whole grammar in one param.*
- Dimension — phrase · riser~ · ride · snap-release at drop · build
- Vertex Size — beat · kick · impulse · a5/d400 · groove+drop
- Rotate ZW Speed — bar · energy · cont (4D rotation only wakes with the track)
- Window — phrase · lfo32 · cont
- Hands: Line, Distance, Scale, Speed, Animate, Show Vertices, Rotate XY/XW.

### Strange Attractor (Sim) — draft-unjudged
*Signature: **Chaos rides the build** — order unravels as the riser climbs; at the drop, Attractor Type steps: a new world, not more of the old one.*
- Chaos — phrase · riser~ · ride · snap-release at drop · build
- Attractor Type — section · §step† (today: pad) · step
- Diffusion — phrase · inv-energy · cont (quiet = soft nebula; loud = etched)
- Speed — bar · energy · cont
- Tilt — phrase · lfo64 · cont
- Hands: Contrast, Scale, Size, Invert. Roll: Particle Count per section energy.

### Fluid Sim 2D (Sim) — draft-unjudged
*Signature: **kick bursts Force through the field** — the fluid takes the hit and carries it; silence lets Anti-Clump regroup the ink.*
- Force — beat · kick · impulse · a5/d600 · groove+drop
- Anti-Clump — phrase · inv-energy · cont (the quiet-section regroup)
- Turbulence — texture · dens~ · cont
- Contrast — bar · energy · cont
- Clip Trigger — pad
- Hands: Flow, Curl, Speed, Fill, Feather, Scale. Roll: Particle Count.

### Fluid Sim 3D (Sim) — draft-unjudged
*Signature: as Fluid 2D, plus **Flatten as the section move** — the volume collapses to a plane when the arrangement empties out.*
- Force — beat · kick · impulse · a5/d600 · groove+drop
- Flatten — section · §class† (today: pad) · step (quiet = flat, drop = full volume)
- Anti-Clump — phrase · inv-energy · cont
- Turbulence — texture · dens~ · cont
- Rotate Y — phrase · lfo64 · cont (H8)
- Cam Dist — phrase · riser~ · ride · build
- Hands: Flow, Curl, Speed, Contrast, Size, Container, Container Scale, Rotate X/Z, Fill, Feather. Roll: Particle Count.

### Particle Text (Text & Media) — draft-unjudged
*Signature: **the drop blasts the message apart; quiet re-assembles it.** Force on kick, Text Strength on inverse energy — the words are only readable when the music lets them be.*
- Force — beat · kick · impulse · a5/d600 · drop
- Text Strength — phrase · inv-energy · cont · a500/r2000
- Turbulence — texture · dens~ · cont
- Contrast — bar · energy · cont
- Hands: Text Size (H5 — the message), Flow, Curl, Speed, Fill, Anti-Clump, Feather, Scale. Roll: Particle Count.

### Black Hole (Sim) — draft-unjudged
*Signature: **the build falls toward the horizon** — Cam Dist rides the riser inward; Freefall is the drop, held for the section, not the hit.*
- Cam Dist — phrase · riser~ · ride (approach) · snap-out at drop · build
- Freefall — section · §class† (today: pad) · step · drop
- Disk Glow — bar · Low · cont (the disk burns with the sub)
- Turbulence — texture · dens~ · cont
- Spin — section · §step† · step
- Hands: Steps, Scale, Tilt, Rotate, Stars, Disk Inner/Outer, Particles, Cam Velocity (H8/H5).

### Nested Cubes (Geometry) — draft-unjudged
*Signature: **kick scatters the nest** — the cubes panic outward and re-nest in the decay.*
- Scatter — beat · kick · impulse · a5/d500 · groove+drop
- Filter — bar · energy · cont
- Speed — bar · energy · cont · a250/r1500
- Mode — section · §step† · step
- Hands: Scale. Pad: Clip Trigger.

### Lissajous (Geometry) — draft-unjudged
*Signature: **frequency ratios step like chord changes** — whole-ratio figures walk a curated table at phrase boundaries; between changes the figure holds, it doesn't wander.*
- Freq X/Y Rate — phrase · §step† (today: pad via Clip Trigger) · step through curated ratio table
- Vertex Size — beat · kick · impulse · a5/d400 · groove+drop
- Phase Rate — bar · energy · cont
- Window — phrase · lfo32 · cont
- Hands: Line, Speed, Scale, Animate, Show Vertices.

### Duocylinder (Geometry) — draft-unjudged
*Signature: **vertices flash on the kick while the line body stays calm** — counterpoint inside one figure.*
- Vertex Size — beat · kick · impulse · a5/d400 · groove+drop
- Rotate ZW Speed — bar · energy · cont
- Window — phrase · lfo32 · cont
- Hands: Line, Distance, Scale, Speed, Animate, Show Vertices, Rotate XY/XW.

### Wireframe (Geometry) — draft-unjudged
*Signature: as Duocylinder — kick lights the vertices; the shape itself only changes when the arrangement does.*
- Vertex Size — beat · kick · impulse · a5/d400 · groove+drop
- Rotate X/Y Speed — bar · energy · cont
- Shape — section · §step† (today: pad) · step
- Hands: Line, Scale, Rotate Z.

### Metallic Glass (Sim) — draft-unjudged
*Signature: **the sub dents the surface** — Displace on Low; treble glints the edges. A material that listens, not a screen that flashes.*
- Displace — bar · Low · cont · a100/r800
- Edge Strength — texture · High · cont
- Light Intensity — bar · energy · cont
- Camera Orbit — phrase · lfo64 · cont (H8)
- Hands: Feedback, Noise Scale/Speed, Mirror, Roughness, Camera Dist/Tilt/FOV, Look Y. Roll: Feedback per section.

### Oily Fluid (Sim) — draft-unjudged
*Signature: **iridescence blooms into the drop** — Chroma rides the riser; the kick slaps the surface (Velocity Displace), and Hue only turns when the section does (H4).*
- Chroma — phrase · riser~ · ride · release at drop · build
- Velocity Displace — beat · kick · impulse · a5/d400 · groove+drop
- Relief — bar · Low · cont
- Contrast — bar · energy · cont
- Hue — section · §step† · step
- Hands: Speed, Feedback, Noise, Velocity Damp, Curl, Color Displace, Saturation, Brightness, Mode.

### Digital Plants (Geometry) — draft-unjudged
*Signature: **Morph rides the build** — the plant re-grows into its other body as the riser climbs.*
- Morph — phrase · riser~ · ride · build
- Petal Amplitude — bar · Mid · cont
- Animation Speed — bar · energy · cont · a250/r1500
- Camera Orbit — phrase · lfo64 · cont
- Hands: Noise Scale, Base Radius, Height, Taper, Torus Radius, Rotation Speed, Box Scale, Camera Dist/Tilt/FOV (H5/H8).

### Plasma (Pattern) — draft-unjudged
*Signature: **Complexity folds tighter into the drop** — the field knots itself as tension rises.*
- Complexity — phrase · riser~ · ride · release at drop · build
- Pattern — section · §step† · step
- Speed — bar · energy · cont
- Contrast — bar · energy · cont (fan with Speed = one voice)
- Hands: Scale. Pad: Clip Trigger.

### Star Field (Pattern) — draft-unjudged
*Signature: **the hats make the sky sparkle** — Twinkle on treble; the field rushes when the track leans in.*
- Twinkle — texture · High · cont
- Drift Speed — bar · energy · cont · a250/r2000
- Brightness — bar · energy · cont
- Hands: Scale, Star Size, Drift X/Y. Roll: Density per section.

### Concentric Tunnel (Pattern) — draft-unjudged
*Signature: **a ring is born on every kick** — the tunnel is the beat made spatial. Rate BPM-locks at roll time.*
- Clip Trigger — beat · kick · trigger (ring birth via Trigger Mode)
- Ring Spacing — bar · Low · cont
- Rate — roll · BPM-locked beat division
- Hands: Line, Shape (section: §step† candidate).

### Basic Shapes (Pattern) — draft-unjudged
*Signature: **the shape snaps solid on the kick** — Fill steps on the hit; the figure is the pulse.*
- Fill — beat · kick · step (cycle fill states)
- Clip Trigger — section · §step† (today: pad) · step (shape change)
- Hands: Line, Scale.

### MRI Volume (Text & Media) — draft-unjudged
*Signature: **the scan is the phrase** — Position ramps through the body over 8 bars; the slice you're in IS where you are in the music.*
- Position — phrase · beat32 · ride (loop per phrase)
- Sharpen — texture · High · cont
- Width — bar · inv-energy · cont (drops cut the slab thin and definite)
- Invert — section · §step† · step
- Folder — roll · per-section body region
- Hands: Center, Scale.

### Text (Text & Media) — draft-unjudged
*Signature: **none — deliberately silent.** Text is the message layer; it does not dance (H5). Restraint is a card too.*
- Hands: everything.

---

# Shipped effects

Effects are spice: most earn one wire or none, and every mapped effect row spends the layer stack's H7 voice budget. An effect whose card is all-hands is *meant* to sit still while the generator moves.

### Bloom (Filmic) — draft-unjudged
*Signature: the glow leans in with the track — one wire, barely visible, felt not seen.*
- Amount — bar · energy · cont · a15/r800

### Transform (Spatial) — draft-unjudged
*Signature: **the zoom-punch** — the frame jolts ~2% on the kick and settles fast. The classic VJ hit, kept tiny.*
- Zoom — beat · kick · impulse · a5/d200, depth ≤2% · groove+drop
- Hands: X, Y, Rotation.

### Chromatic Aberration (Filmic) — draft-unjudged
*Signature: **the thump fringes the frame** — color splits for a blink on the kick.*
- Amount — beat · kick · impulse · a5/d250 · groove+drop
- Hands: Offset, Mode, Angle, Falloff.

### Color Grade (Color) — draft-unjudged
*Signature: **the color drop** — Saturation drains through the build and slams back at the drop. Hue turns only with the section (H4).*
- Saturation — phrase · riser~ · ride (drain) · snap-back at drop · build
- Contrast — bar · energy · cont
- Hue — section · §step† · step
- Hands: Amount, Gain, Colorize, Tint Hue/Saturation/Focus.

### Strobe (Stylize) — draft-unjudged
*Signature: **played, not wired** — Rate BPM-locks at roll time; Amount stays a hand fader or pad. Drop-only (H9).*
- Rate — roll · BPM-locked division
- Amount — pad / hands · engage drop
- Hands: Mode. Pad: Clip Trigger.

### Glitch (Filmic) — draft-unjudged
*Signature: **the snare breaks the picture** — transient impulse, new block pattern every hit. Build+drop only (H9).*
- Amount — beat · trans · impulse · a0/d150 · build+drop
- Block Size — beat · trans · random per hit · build+drop
- Hands: RGB Shift, Scanline, Speed.

### Edge Stretch (Spatial) — draft-unjudged
*Signature: **the frame tears open into the drop** — Amount rides the riser, snaps shut on the downbeat.*
- Amount — phrase · riser~ · ride · snap at drop · build
- Hands: Width, Direction (roll).

### Depth of Field (Filmic) — draft-unjudged
*Signature: **rack focus at the boundary** — focus steps when the section does; the blur lives in quiet sections.*
- Focus — section · §step† · step
- Width — phrase · inv-energy · cont
- Engage: quiet+build (Amount hands elsewhere)
- Hands: Amount, Mode, Focus X, Blur, Angle, Quality.

### Highlight Boost (Filmic) — draft-unjudged
*Signature: the treble lifts the highlights — bright sounds make bright pixels.*
- Amount — bar · High · cont
- Hands: Gain, Threshold, Knee.

### Soft Focus (Stylize) — draft-unjudged
*Signature: quiet sections breathe soft — the image relaxes when the music does.*
- Amount — phrase · inv-energy · cont · engage quiet
- Hands: Radius.

### Watercolor (Stylize) — draft-unjudged
*Signature: a quiet-section skin — pigment jitters with the hats, blooms in the stillness.*
- Displace — texture · dens~ · cont · engage quiet
- Amount — phrase · inv-energy · cont · engage quiet
- Hands: Blur, Decay.

### Dither (Color) — draft-unjudged
*Signature: lo-fi texture that blooms in breakdowns and vanishes when the track fills.*
- Amount — phrase · inv-energy · cont · engage quiet
- Pattern — section · §step† · step
- Hands: —.

### Digital Drift (Filmic) — draft-unjudged
*Signature: **transients kick the signal loose** — RGB tears on hits over a drift that tracks the energy.*
- RGB Shift — beat · trans · impulse · a0/d200 · build+drop
- Drift — bar · energy · cont
- Bands — section · §step† · step
- Hands: Speed.

### Kaleidoscope (Spatial) — draft-unjudged
*Signature: symmetry is section-weight (H2) — Segments steps at boundaries, never on the kick.*
- Segments — section · §step† (today: pad) · step
- Hands: Amount.

### Voronoi Prism (Stylize) — draft-unjudged
*Signature: the shatter tightens into the drop — cell count steps up each phrase of the build.*
- Cells — phrase · §step†/riser~ · step per phrase · build
- Amount — phrase · riser~ · ride · build
- Hands: Cell Size.

### Infrared (Color) — draft-unjudged
*Signature: a whole-section look — heat palette steps with the arrangement, not the beat.*
- Palette — section · §step† · step
- Contrast — bar · energy · cont
- Hands: Amount (section engage).

### Invert (Color) — draft-unjudged
*Signature: **the frame flash** — full invert on the drop downbeat, one beat long, then gone (H9). Rare by design.*
- Amount — beat · §bound†-downbeat (today: pad) · step-flash, ≤1 beat · drop only
- Hands: —.

### Mirror (Spatial) — draft-unjudged
- Mode — section · §step† (today: pad) · step
- Hands: Amount.

### Quad Mirror (Spatial) — draft-unjudged
- Amount — section · §step† (today: pad) · step
- Hands: —.

### Stylized Feedback (Stylize) — draft-unjudged
*Signature: the tunnel breathes with the sub — continuous, bar-weight, never a kick swell (H1).*
- Zoom — bar · Low · cont · a100/r1000, subtle depth
- Rotate — phrase · lfo64 · cont
- Hands: Amount (section engage).

### Wireframe Depth (Diagnostic) — draft-unjudged
*Signature: the depth mesh pumps with the sub while density holds phrase-steady.*
- Z Scale — bar · Low · cont
- Density — phrase · §step† · step
- Hands: Amount, Width, Smooth, Subject, Blend, Edge Follow.

### Edge Detect (Diagnostic) — draft-unjudged
*Signature: **the build strips the image to its bones** — edges-only as the riser peaks, full frame restored at the drop.*
- Amount — phrase · riser~ · ride · snap-off at drop · build
- Hands: Threshold, Mode.

### Auto Gain (Stylize) — draft-unjudged
*Already audio-reactive by construction — card is roll-time config only.*
- Roll: Target, Sensitivity, Ratio per section energy. Hands: Amount.

### Blob Track (Diagnostic) — draft-unjudged
*Utility/diagnostic look — no musical wiring proposed.*
- Hands: everything.

### Color Compass (Spatial) — draft-unjudged
*Reactivity is built into the effect — roll-time config only.*
- Roll: Reactivity. Hands: Intensity.

---
