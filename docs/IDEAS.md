# Ideas

Features worth building that nobody is working on. Each one predates today's code, so design it fresh against the current code before building it.

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

## Simulation

### GPU sand, snow and goo solver
A GPU material solver for sand, snow, goo and lava, so these can play as instruments. Already built: the early steps. The flagship materials and most stages are still open.

### Cloth and rope simulation
Lets cloth and ropes swing and drape live in the scene, and could move with the beat. Nothing solves cloth or rope motion yet.

### Replay recorded GPU work every frame
Records the GPU work of a repeated section once and replays it each frame, cutting the CPU cost of repeated steps and leaving more room in each frame. Already built: part of it is on the main line. The fluid step is still open.

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
