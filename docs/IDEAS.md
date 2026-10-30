# Ideas

Features worth building that nobody is working on. Each one predates today's code, so design it fresh against the current code before building it.

Where an idea builds on someone else's work, its entry credits them and names the licence. MIT, BSD and Apache code can be ported once its notice goes in THIRD_PARTY_NOTICES.md, and Apache also asks that changed files say so. GPL code can't go into a closed commercial app, so those ideas get rebuilt from the published maths, never from the code.

## Live performance and sync

### Import an Ableton set as a show score
Pick an Ableton Live set and get its tempo map, one layer per track, cues from the locators, and trigger clips from every MIDI note in one step. Re-importing after the arrangement changes in Live would update the show and keep your manual edits.

### Session mode grid
A grid of layers by scenes that launches on the beat, like the clip view in Ableton, for playing clips live. Already built: the launch commands and session state. The grid screen and recording are still open.

### Show preparation
Loads the whole show ahead of time so no clip, scene or video stutters the first time it plays. It would replace the current warm-up timeout with a real readiness check. Already built: some warm-up already runs in the app.

### Gig resilience against crashes and hardware faults
Brings the app back fast after a crash so the screen stays lit and the music keeps going, and warns about overheating or GPU faults. Already built: some crash logging. The heat warning and fault display are still open.

### DJ mode over Ableton with a music library
Browse a library of your own analysed tracks with tempo, key and stems, and run two decks through Ableton the way a DJ works a booth. The visuals would follow whichever track is playing.

### Pro DJ Link sync
Lets the visuals follow the CDJs and the DJ mixer over the network, and start visual scores from the track that is playing. Needs Perform surface first.

### Perform surface
A simple stage screen where you set up the controls you need during a show, with the timeline using the same widgets as the rest of the UI. Nothing is built yet.

## Audio

### Measured accuracy for audio analysis
Scores each detector against public music datasets, so you know how well the kick, snare and hat finders work instead of guessing. The payoff is cleaner clips from a track with fewer false hits. Already built: the tuned defaults. The sustained-object work and final clean-up are still open.

### Per-clip audio detection settings
Select one audio clip, tune its own detection settings, then press Detect to put trigger clips on the layers you pick. Each clip keeps its own settings instead of one global import wizard. Already built: the data model and some inspector wiring. The full flow is still open.

### Audio object ingest for timeline clips
Makes the clips that appear on the timeline when you drop a track in, using the same object tracker that drives modulation, so the clips sit on the real notes and hits.

### Audio object tracking as a modulation source
Follows the loudest voice in a sound over time and turns it into a control you can wire to visuals. Already built: most of the tracking. Still open: the scope overlay and a fix in the gap ring chase.

### Track-wide visuals that feel musically chosen
Drop in a track, let the app find its sections such as build and drop, and fill each one with a varied set of generators and effects that you can re-roll. Audio links would follow musical rules, such as a kick owning one parameter and events hitting as quick pulses, with notes for each preset so the picks feel chosen by a person. Already built: the modulation matrix and kick events.

## 3D scenes and rendering

### Blender and glTF scene import
Brings in 3D scenes made in Blender and plays them on stage. Already built: the glTF importer. Baked caches, texture sets, and the Resolume and TouchDesigner import paths are still open.

### Import any 3D model on drag and drop
Drag in a model you bought and it loads and looks reasonable, with a report line for anything the app does not support yet. No model should fail silently.

### glTF extension coverage roadmap
Covers the 3D model extensions that common assets use, ranked by what to support next. The app would also warn when a model uses an extension it does not know, instead of failing without a word.

### glTF material extensions for glass, fabric and metal
Makes glass, translucent plants, velvet, sheen and stretched metal look right on imported models instead of looking grey. Some of these are in progress, and the shading work is still open.

### Gaussian splat scans as a playable source
Loads photoreal 3D scans made of splats and drives each splat's position, size, colour and opacity from the same controls as everything else. A scanned object could then move with the music.

### Splat modifiers
Lets a scanned point cloud ripple, open and reform with the same math as the mesh modifiers. Needs Gaussian splat scans as a playable source first.

### Scene mesh decimate
A card that thins the polygons on one object or a whole scene, so heavy meshes run smoothly on stage. The approach needs rethinking before anyone builds it.

### Scene echoes and attraction
Turns a moving object into a trail of copies, or makes objects gather and then let go, so motion becomes shape. Already built: the spatial echo modifier. Attraction and recorded motion history are still open.

### Math fields for scene motion
Waves and formations that move objects in a chosen pattern, driven by your controls. Already built: surface waves and the band and sphere masks. The wider set of fields is still open.

### Mesh assembly
Breaks one mesh into pieces that fly apart and then come back into shape. Already built: cutting by band and by cell for the fragment modifiers. Free morphing between shapes is still open.

### Merge modifier for 3D objects
A scene modifier that melts two 3D objects together where they touch, like organic growth or metaballs, with the light on the blend handled correctly. Already built: the scene modifier framework it would plug into.

### Rendering baseline v2
Pushes 3D scenes toward a cinematic look with a richer render buffer, a camera and lens model, and a post stage. The high dynamic range part is still open.

### Ray tracing motion denoise
Keeps the ray traced picture clean when things move. Already built: the still-scene denoise filter, which gets weak under motion. The history-aware version is still open.

### Ray tracing tier 3 scoping
A list of the next ray tracing features, such as reflections on shiny surfaces, to decide which ones get built. Each item needs its own plan before any code is written.

### 3D fractals you can play
Mandelbulbs, Mandelboxes and other fractals rendered live and morphing on the beat. A fractal is already a distance field, which is what our colliders read, so water and sand could pour over one. Inspired by [Mandelbulber](https://github.com/buddhi1980/mandelbulber2) by Krzysztof Marczak and contributors (GPL-3.0), so build from the published formulas, not its code: the Mandelbulb by Daniel White and Paul Nylander, the Mandelbox by Tom Lowe. Nothing is built yet.

### Pen plotter 3D lines
3D scenes drawn as clean ink lines with the hidden edges removed, like a pen plotter drawing. From [ln](https://github.com/fogleman/ln) by Michael Fogleman (MIT). Already built: wireframe edges, but they show the edges behind too.

### Gems with rainbow fire
Diamonds and crystals that throw rainbow sparkle as light bends and splits inside them, as part of the material upgrade. Inspired by [diamond-webgl](https://github.com/piellardj/diamond-webgl) by Jérémie Piellard (GPL-3.0), so build it from the optics, not the code. Already built: glTF's dispersion value loads, but nothing draws it yet.

### Occlusion culling for geometry-heavy scenes
Skips geometry hidden behind other geometry: a depth pyramid from last frame's visible patches, then a second pass that re-tests the rest against this frame's, so nothing drops on a cut. It slots into the patch cull (`SCENE_GEOMETRY_MAP.md`). On the Mac the measured landscapes are pixel-bound, so it pays only for a scene that profiles as vertex-bound.

### Terrain tiles by distance
Draws the terrain as a quadtree of fixed-size tiles at a resolution set by screen error, neighbours at most one level apart with edges snapped so no cracks open, read from the terrain's height map. It replaces the single 512² grid when terrains grow past what one grid covers, or a profile shows terrain vertex work.

### Cluster ray tracing on NVIDIA
With the Vulkan ray tracing port on the 5080: per-cluster acceleration structures (`VK_NV_cluster_acceleration_structure`, RTX Mega Geometry) and a partitioned top level, so huge scanned and changing scenes trace without rebuilding whole meshes.

## Simulation

### Snow, goo and lava on Matter
Snow, goo and lava on Matter, so these can play as instruments. Sand and soil are designed in MATTER_SAND_DESIGN.md, and its material stage is where these join: snow needs hardening and costs about 124 steps a frame when packed, goo needs a shape memory, lava a yield stress and melt.

### Landslides, collapsing structures and splats as physics, on one grid
Cinematic material scenes that react to the music: a mountainside giving way with boulders tumbling through the dirt, a scanned building collapsing into chunks on a drop, splat scans that bend, crack and flow. Film builds these in four layers, all on the water's shared grid (WATER_SHARED_GRID_DESIGN.md), so a slide can run into a lake and every material touches every other. First, the bulk soil in Matter as a sand and soil material with cracking. Second, tens of big boulders as Box3D bodies coupled two ways with the soil, the way coupled bodies float in water today. Third, thousands of small rocks, branches, props and pre-broken building chunks in a new GPU debris solver that collides through the grid's distance fields. Fourth, gravel spray and dust thrown off the soil the way whitewater is thrown off water (millions of cheap one-way particles), with the dust cloud in the planned gas solver on the same grid. Splats become physics by turning each splat into a Matter particle (PhysGaussian, 2024) or pinning splats to rigid chunks. Beat-reactive by design: hits fire impulses and quakes through the existing force lattice, and a drop can trigger the collapse. Already built: Matter with stiffness and cohesion on world tiles, Box3D bodies coupled to water and Matter, whitewater-style secondary particles, splat playback. Needs first: the shared grid, Matter on it, a soil material, the debris solver, dust, and finer sampling of large photoscan colliders (BUG-js61b). Full detail is an overnight export; live runs coarser and leans on motion blur, dust lighting and depth of field.

### A living world you play
One photoreal world where terrain, water, mud, sand, plants, rocks, debris, dust, smoke and fire all sit on the World's one physics grid, so everything touches everything else. You don't animate it, you play its forces: the kick is a quake, the snare a blast, pads are wind, the build lifts a mountain range, the drop floods the basin. It stays affordable because everything sleeps until something disturbs it: ground, rocks and trees cost nothing until a blast or a wave reaches them.

The layers, ground up. Terrain: Inigo Quilez's eroded-looking noise for the shape, which morphs live, then grid erosion that updates every cell at once (Mei et al. 2007) so the rivers you see are the ones carving the land; drawn as a mesh, not raymarched, so it gets shadows, reflections and wetness; scanned CC0 rock, soil, grass and snow blended by slope, height and water flow. Plants: trees from branching rules like [EZ-Tree](https://github.com/dgreenheck/ez-tree) by Dan Greenheck (MIT), which give the skeleton for free; a few varieties copied thousands of times, each a handful of springs that bend with the grid's flow and snap past a limit; grass made fresh on the GPU each frame near the camera, as in Ghost of Tsushima. Rocks: scanned, asleep until hit, then rigid bodies. Air: spray and dust thrown off the ground, smoke and fire in the gas solver. Far things draw simpler, down to flat cards.

Where else it goes: rain that fills rivers and turns soil to mud, snow that avalanches, forests growing live over a build, landslides and explosions, the same plants redrawn in the nonflowers ink style, fractals rising out of the hills, splat scans of real places with terrain past their edges, Chladni patterns across a landscape, and drone flyovers played live. At the end, a player character on an Xbox controller running through an endless world: it needs gamepad input (which would also drive show parameters), a character controller, walk and run animation switching, a follow camera, and a world that recentres as you travel.

Already built: the World's one grid and memory budget, water, sand that water wets, drags and soaks into, mud, snow, clay, goo and jelly, smoke and fire that flow round solids, rigid bodies, materials with parallax, and a Terrain shape with eroded-looking noise. Needs: grid erosion, a terrain material that layers by its masks, more detail near the camera, a height-field collider that reads the ground directly (water, sand and gas read colliders through `LiquidBodies`/`SharedSolids` in manifold-water-liquid), plants with spring skeletons, the debris solver, and ground dust. Open risks: whether dense plants fit the frame budget, which needs a same-scene measurement; close-up leaves, the hardest part to make look real.

### Chladni sand patterns
Sand on a vibrating plate settling into patterns that change with the pitch of the music, as Ernst Chladni showed in 1787. Working out how a plate of any shape vibrates is finite element maths, the kind [scikit-fem](https://github.com/kinnala/scikit-fem) by Tom Gustafsson and contributors does (BSD-3-Clause); the sand then gathers on the lines that stay still. Already built: Matter sand.

### Different sim speeds in one scene
Two waters, or one water and the bodies in it, running at different Speeds and still touching: a pool in slow motion beside one at full speed, a slow-motion splash round a body that falls at normal speed. Today a Water Speed other than the World's stops coupled water with an error (BUG-n0nr), because the water and Box3D must step the same clock. On the shared grid (WATER_SHARED_GRID_DESIGN.md) each region would carry its own time scale, and exchanges between regions would convert momentum across the seam. Needs first: the shared grid, then a clock per region with a rule for what a body straddling two speeds feels.

### Regional time steps for packed snow
Packed snow takes its own short steps inside Matter's shared grid while sand, mud and jelly keep longer ones (Fang, Hu, Hu and Jiang 2018, AsyncMPM: power-of-two steps per region, coarse first, the coarse points' last state as the buffer). The payoff is bounded: packing stiffens snow at most 5×, so its waves run at most 2.2× faster and it at most doubles the step count. Other materials can only ever take double steps, and only when the ratio clears 2 (fully packed snow beside sand in a 4 m domain is 2.03; taller domains or looser snow fall under it, unless the snow's step shrinks to make it exactly 2). Matter could run nearly twice as fast with a small packed snowball in a lot of sand; snow-heavy scenes barely change. Needs: per-tile levels at each rebuild, per-level tile lists for the grid passes, a ~110-byte backup per coarse point, and G2P skipping other levels' points.

### Cloth and rope simulation
Lets cloth and ropes swing and drape live in the scene, and could move with the beat. Nothing solves cloth or rope motion yet.

### Coarse water with small helper networks for fine detail
Runs the GPU FLIP water on cells two or more times larger, which gives about 8× fewer cells per doubling, and lets small networks add back what the small cells would have caught: surface tension on drops and thin sheets, the swirl that coarse grids smear away, and fine surface ripples. The real solver stays in charge of pressure and volume and each network only adds a correction, so a wrong guess costs detail instead of breaking the sim. The physics is built into each network's structure: the swirl network outputs a field whose curl is the velocity change, so it can't add or remove water; corrections are predicted as flows between neighbouring cells, so momentum is kept; surface tension takes its direction from the surface normal and the network only predicts the strength from coarse curvature; added swirl is capped at the energy the coarse grid smeared away; and inputs are scaled by cell size, time step and fluid properties so one network holds across Cell Size and Sim Rate. Soft rules go in the training loss: smoothness over time, and matching the fine sim's spread of eddy sizes. The networks run as passes on the shared grid, not inside the water code: they read and correct grid velocities in small local patches with material properties as inputs, so any solver that moves particles through the grid (GPU FLIP water, Matter's sand, snow and goo) can use them. The grid pass, patch shape, coarse/fine training pipeline and shader runner are shared; each network is per effect (swirl for liquids, grain friction for sand). Surface tension, thin walls, sub-cell droplets and horizon water are solved directly first (epic BUG-chfj), so the networks only learn what those can't describe. Training data comes from running the same scenes coarse and fine on the 5080, and training runs the coarse solver in the loop so the networks learn to fix their own drift. In the show each network runs as a small hand-written compute shader with built-in weights, like the audio detector's CNN. Open risks: flicker from frame to frame, and whether the networks cost less than the cells they save, which needs a same-scene measurement.

### Replay recorded GPU work every frame
Records the GPU work of a repeated section once and replays it each frame, cutting the CPU cost of repeated steps and leaving more room in each frame. Already built: part of it is on the main line. The fluid step is still open.

## Generators and effects

### Growing images that heal
An image grows from a single seed, a kick tears a hole in it and it grows back. Each pixel runs a tiny trained network, so it's cheap live, but each image needs training first. From Growing Neural Cellular Automata by Alexander Mordvintsev, Ettore Randazzo, Eyvind Niklasson and Michael Levin (Distill, 2020), and the [PyTorch version](https://github.com/PWhiddy/Growing-Neural-Cellular-Automata-Pytorch) by Peter Whidden (Apache-2.0). In the show it would run as a small shader with built-in weights, like the audio detector's network.

### Reactive Game of Life
John Conway's Game of Life (1970) where kicks seed new cells, snares wipe areas and the incoming image decides where life starts. A small shader, next to the Reaction Diffusion preset.

### Light as waves and rays
Two looks from optics. Waves: light rippling and interfering through slits and lenses, as [MEEP](https://github.com/NanoComp/meep) by MIT's Nanostructures and Computation Group simulates (GPL-2.0, so rebuild from the wave equations). Rays: laser beams bending through prisms and mirrors and splitting into colours, as [Ray Optics Simulation](https://github.com/ricktu288/ray-optics) by Yi-Ting Tu draws (Apache-2.0). Nothing is built yet.

### Growing lines
A line that grows, pushes itself apart and folds into coral and brain shapes. From [differential-line](https://github.com/inconvergent/differential-line) by Anders Hoff, who works as inconvergent (MIT).

### Ink painting plants
Plants and flowers grown from code in the look of Chinese ink painting. A look Peter especially wants. From [nonflowers](https://github.com/LingDong-/nonflowers) by Lingdong Huang (MIT).

### Live seam carving
Squeezes and stretches the picture by cutting out its least important seams, so faces and edges hold while everything else melts. After a feedback node it would get very trippy. Seam carving is by Shai Avidan and Ariel Shamir (2007), and [caire](https://github.com/esimov/caire) by Endre Simo is a clean reference (MIT). Each seam depends on the one before, so live needs a version built for the GPU.

### Particles drawn as density
Millions of particles drawn as glowing density with the brightness spread evenly, like a long exposure, instead of as dots. The technique is from [datashader](https://github.com/holoviz/datashader) by HoloViz (BSD-3-Clause). It would work on any of our particle sims.

### Window portals
A 2D effect where the picture only shows through fake desktop windows with title bars, buttons and shadows, which open, move and close on the beat, in a TouchDesigner style. Each window could show the layer behind it or a different layer. Nothing is built yet.

### Redacted text
Black bars that sweep over text, cover words and lift off again, like a declassified file. Pairs with Code Terminal. A Redaction name is still in the effect ID list, but no effect is behind it.

### Code Terminal that types nonstop
Code Terminal should look like someone typing nonstop down a terminal window: the cursor runs line after line and writes over the old text as it goes. Today it retypes whole 8-row blocks on the beat instead.

## Video and outputs

### Syphon and NDI live video
Sends the show to Resolume, TouchDesigner or OBS, and takes live video in from them. Syphon comes first. Nothing is in the code yet.

### LED strips as a stage output
Lets you treat LED strips as another output and play them with the same clips and patterns as the screen. Already built: the LED layer, nine patterns and a DMX lane preview. Still open: strip layout mapping, network DMX output, and a few MIDI and drag-to-convert touches.

### Multi-display and totem stage model
Lets you treat two LED totems on stage as one canvas, so content can run between them and bounce off them. Already built: the first step. Per-display rendering and multi-output present are still open, and they unlock projection mapping and strip layouts.

### Projection mapping
Warps, masks, slices and edge blends the picture so it fits a real wall or set piece. Needs Multi-display and totem stage model first.

### Media decode and encode traits
Moves video decoding and encoding behind one clean GPU interface, and speeds up HAP and DXV clips and still images. DXV support does not exist in the code yet.

## Timeline and UI

### App menus, settings and shell panels
Fills out the menu bar, splits settings into app preferences and project settings, and puts more real settings within reach. Today the menu stops at File and the settings popup holds only four rows.

### Clip thumbnails as live pictures
Timeline clips show a picture of what they look like instead of a flat colour. Already built: the base system and the small-size picture step. A filmstrip with a live display, capture and saved pictures is still open.

### Timeline drop and paste
Lets you drop an audio file onto an existing layer, paste one from Finder, and replace the audio while keeping the layer. Already built: paste, replace audio and stem tracks. Dropping onto a layer while dragging from Finder is still open, because the drag tracking freezes mid-drag.

### Timeline and layer header redesign
A clearer timeline with solid coloured layer headers that read at a glance on a dark stage.

### Multi-selection inspector
Select many clips or layers and change them together. Already built: batch delete, collapse and expand, and group trim. The inspector that shows a mixed value when the selection differs is still open.

### UI craft and motion polish
A finish pass on spacing, timing and motion so the controls feel smooth on screen. Already built: the motion code and tests in the overlay.

### Design colours, sizes and inspector redesign
One shared set of named colours, sizes and spacing for the whole UI, plus a cleaner effect card in the inspector. Already built: some layout changes. The shared set and the card redesign are still open.

### Clip and layering rules for panels
Stops panels spilling past their edges or drawing over each other, which can hide controls in the middle of a set. Already built: the main window has the first part. The editor window and the perform view still need the same rules.

## Graph and nodes

### Component library of reusable graph pieces
Save a group of graph nodes as a named component and reuse it in other patches. Your own building blocks sit in the library, and the app's tools can use them too.

### Node group naming and colour
Name a group and give it a colour so a busy graph is easy to read. Already built: navigating into groups, marquee select and collapse. Naming and colour are still open.

### Graph editor and inspector as one surface
Makes the graph editor and the main inspector share the same controls, so selecting a card in the main view brings it up in the editor. Already built: the selection link and the full inspector column, while fitting every window width is still open.

### Graph editor layout and node redesign
Compact nodes, previews that show what each node makes, resizable panels, coloured wires, and a small timeline at the bottom for time-based patches. A patch becomes easier to read while you build it, and nothing is built yet.

### Graph and parameter UI polish
Makes effect, generator and graph controls easier to read and edit, with on-node sliders and in-place renaming. Much of this is already in the app.

### Machine learning nodes
Lets a neural net read the camera or a clip and feed depth, person masks or body points into the graph. Already built: a person segment node and a depth estimator. The wider node set is still open, and the heavier tier waits on the new GPU backend.

### Visual piece graph drafts
A set of ready-to-build visual pieces, each with its graph and a performer card, so you can pick which ones to build. Nothing is built yet.

### Effect masks for partial wet and dry
Paint a mask so an effect only hits part of the picture, with the mask coming from shapes, the incoming image or another layer, for sidechain style control on stage. Already built: masks, modifier groups and two new generators. Contour masks and audio-driven masks are still open.

### One value per parameter in the project
Every parameter stores two numbers in the project: the value you set (`base`) and the value playing right now after LFOs, audio and automation (`value`), with a per-effect `base_tracked` flag saying whether `base` is used at all. That split is why `get_param` and `get_base_param` both exist and get mixed up. The project should keep only your value, with the playing value living in the engine's output. It touches the core data model, playback, modulation and the save format, and old shows must still load. Do it after the single-truth project state work lands.

## Product

### Commercial release, trial watermark, updates and crash reports
An offline unlock file, a watermark on trial video, an update check that never runs in perform mode, and opt-in crash reports. None of this is in the code yet.

### MCP interface for AI assistants
Lets an AI assistant such as Claude Desktop drive the app over a local connection, so you can build a scene by asking for it. Nothing in the code is an MCP server yet, and it depends on the node naming cleanup being finished first.

### UI automation
Lets a script find, click and check controls in the running app, so UI bugs get caught without someone reading screenshots by eye. Already built: the basic control and flow pieces. Wider flows and multi-window support are still open.

### Lava crust that travels with the lava
A textured Matter (lava, mud, gravel) reads its texture fixed in the world, so while it flows the surface slides under the crust. The fix is texture coordinates carried by the particles: each point keeps where it started, the surface vertices take it from the points round them, and the box mapping reads there. The crust then moves and stretches with the material, and the stretch can open glowing cracks. Flow maps, which crossfade two copies sliding along the surface velocity, double and streak the cracks and look worse than the fixed crust.
