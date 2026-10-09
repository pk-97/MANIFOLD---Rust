# Scene panel vs inspector cards

Both surfaces read `PresetInstance.params` and send edits through the content
thread. The graph remains authoritative for ownership and connections; neither
surface keeps a separate scene model or parameter store.

## Inspector

Scene generator cards project the same manifest rows into a fixed order:

1. Camera: Horizontal Angle, Vertical Angle, Distance, Field of View.
2. Lighting: Exposure, Environment Strength, Main Light Intensity.
3. Depth of Field: On/Off, Focus Distance, Aperture.
4. Motion Blur: On/Off, Shutter Angle.

Unavailable controls remain in place, disabled and unmappable. Available rows
retain their parameter IDs, values, ranges, mappings and modulation. Sliders
remain usable while an effect is off, allowing settings to be prepared before
enabling it. Explicitly exposed user controls and authored composite macros
follow under Scene Controls. Automatic object, material, quality and per-light
details belong in Scene Setup.

`ui_bridge/projection/scene_performance.rs` selects roles from SceneVm and real
graph bindings. Presentation labels and sections may differ from manifest
labels; storage and command addresses do not. Non-scene cards continue to use
`card_visible`. Per-frame values join the full manifest by ID; unavailable
presentation placeholders deliberately have no live slot.

## Scene Setup

The outliner order is Camera, Lighting & Environment, Objects, Motion & Physics,
Rendering. Object properties order Transform, Material, Modifiers, Physics.
The panel receives `SurfaceVisibility::All`; ownership and category filters
select the relevant rows. Camera controls follow the render camera's actual
connections across groups. Unrelated or miswired effects do not masquerade as
controls for the active camera.

New and bare older scenes use the shared cinematic-tail builder also used by
GLB import. Newly added DoF and motion blur start off to preserve appearance.
Existing authored chains are preserved during load. The Camera page offers
Set Up Camera Effects when dependencies are missing. It restores supported
standard wiring as one undoable content command; ambiguous custom graphs are
left unchanged with a diagnostic.

Scene and object modifiers reuse ParamCardPanel and stable instance IDs through
reorder, duplicate, rename, undo and save/reopen. Successful insertions select
the new item only after the content command succeeds.

## Adding controls

`scene_exposure::migrate_scene_exposures` stamps graph bindings and manifest
descriptors. `RENDER_SCENE_STAMPED_PARAMS` selects renderer parameters available
to Scene Setup. Add a performance control to the scene projection only when it
belongs in the stable high-level layout; adding a primitive parameter does not
automatically add inspector clutter.
