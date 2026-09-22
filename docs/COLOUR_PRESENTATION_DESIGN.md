# Colour presentation — preserve the image, adapt each destination

**Status:** SHIPPED implementation · 2026-09-22 · Codex. Prerequisites: existing native Metal renderer.
**Hardware acceptance:** pending BUG-qdn7; not a claim of verified physical HDR output.
**Execution contract:** follow DESIGN_DOC_STANDARD sections 5–6 and the repository landing gate.

MANIFOLD renders one linear HDR image, then adapts it independently for each
destination. Display detection must not alter the shared image or master-effect
state. UI colours remain ordinary reference-white colours even on an HDR surface.
Peter requested strong ownership and types so small wiring errors cannot silently
change colour interpretation. He selected: “Use the consistent HDR pipeline
(recommended)” after being told that moving display adaptation after master effects
can change legacy SDR looks. He prefers automated/instrumented checks to computer
use and currently has no external display connected.

Related: MANIFOLD_GPU_ARCHITECTURE.md (native ownership), MULTI_DISPLAY_DESIGN.md
(future output multiplicity), MEDIA_EXPORT_MAP.md (media boundaries), and
HEADLESS_UI_HARNESS.md (shared UI rendering). Tracking: BUG-akbg, BUG-y40n.

## 1. Audit — verified 2026-09-22

| Piece | Source | Finding before this work |
|---|---|---|
| UI colour values | manifold-ui/src/node.rs, Color32/LinearColor/TextColor | Distinct geometry/text currencies already exist; reuse them |
| Main window | manifold-app/src/app.rs, resumed/resize_ui_offscreen | Linear BGRA8 buffers and drawable despite EDR colour-space tag |
| Preview | ui_frame.rs, composite_main_ui_frame base pass | Float bridge sampled directly into BGRA8 |
| Output | content_pipeline.rs, set_output_surface/render_content_native | Float drawable; shares compositor tone mapping with preview |
| Display query | edr_surface.rs, query_screen_headroom | Potential value replaces current available headroom |
| State update | content_commands.rs, UpdateEdrHeadroom | Both windows overwrite one unaddressed scalar |
| Master processing | layer_compositor.rs, Compositor::render | Display-dependent tone mapping precedes master effects |
| PNG | ui_snapshot/render.rs, save_bgra_png | Swizzle only; linear bytes saved as ordinary image values |

History: 310fbc05f moved UI caches to BGRA8; dce2f7553 replaced the main
window's float-preferred surface with BGRA8 while retaining EDR setup. No
intentional downgrade rationale was found. This is source history, not proof
of historical hardware behaviour.

## 2. Decisions

**D1. One canonical linear HDR frame.** Content-thread rendering uses
TonemapMode::SceneLinear (exposure, no display curve) before the existing master
effect chain. Stateful effects run once. Presentation occurs afterward.
Rejected: rendering the master chain once per display, which duplicates state and
work; and letting the last display notification select the shared image's look.
Cost: SDR master effects now see HDR values, an explicitly accepted behaviour change.

**D2. One policy owner, separate destination snapshots.** DisplayPresentationState
belongs to ContentPipeline. UI-thread native queries send
UpdateDisplayCapabilities { destination, capabilities } through ContentCommand.
Workspace, Output and GraphEditor are runtime destinations, not serialized project
settings. Output detach removes only Output. No new locks or per-frame allocation.

**D3. Separate meaning from storage.** presentation.rs owns private checked
CurrentHeadroom/PotentialHeadroom types and DisplayCapabilities. DisplayPlan uses
current headroom; potential records capability only. Invalid native values log a
diagnostic and explicitly select SDR. The native provider remains a small adapter;
tests inject capability values into the same policy owner rather than inventing a
second display implementation. Apple documents current-headroom changes as
posting the existing screen-parameters notification:
[maximumExtendedDynamicRangeColorComponentValue](https://developer.apple.com/documentation/appkit/nsscreen/maximumextendeddynamicrangecolorcomponentvalue).

**D4. Float window composition.** UI_FORMAT is RGBA16Float for UI caches,
composition, pipelines and drawables, including graph and perform windows. The
surface remains ExtendedLinearSRGB. Palette values are unchanged. A destination
mapper cannot accept UNORM targets; Metal drawable wrapping checks actual storage.
Cost: these buffers use eight bytes per pixel instead of four. This does not
double the whole engine's memory, and is not a measured performance claim.
Rejected: changing only the drawable, which leaves clipping in the offscreen buffer.

**D5. Reuse tone mapping formulas.** PresentationPipeline uses extracted shared
WGSL curve functions from TonemapPipeline. SDR uses the project-selected curve;
EDR preserves values below a soft shoulder at current headroom. Display-mapped
values remain linear; macOS performs display colour conversion. No unconditional
sRGB encode in the rendering pipeline and no tone mapping of UI palette colours.

**D6. Explicit capture boundary.** LinearUiReadback validates dimensions, storage
and alpha convention. Only its conversion produces SrgbRgba8; the PNG writer
accepts that currency and writes sRGB metadata. Float readback is encoded before
8-bit quantization. Premultiplied alpha is handled in linear light. Filmstrips
assemble already encoded pixels without a second transfer.

**D7. Export is a destination, not a monitor.** SDR video and live recording map
the canonical frame to SDR explicitly. HDR video retains its existing PQ encoder;
still export retains its explicit faithful/rolloff policy. Display attachment or
brightness cannot change the canonical render. No export codec/gamut redesign.

## 3. Interfaces and ownership

Renderer presentation.rs owns UI_FORMAT, the capability value types,
DisplayDestination, DisplayPresentationState, DisplayPlan, LinearSceneFrame,
LinearPresentationTarget, DisplayMappedFrame and PresentationPipeline. Checked
constructors keep texture-format assumptions at the boundary. encode consumes a
scene-frame view, a float target view and a plan; it returns a mapped-frame view.
These are borrowed GPU handles, not copies of images or new shared ownership.
Runtime validation cannot establish what arbitrary shader code wrote; GPU proofs
remain necessary. Colour node previews explicitly establish colour semantics
before accepting sampled colour texture formats; scalar/depth diagnostics retain
their existing visualization policies.

The app owns windows and screen notifications. The content thread owns policy
snapshots, conversion resources and the canonical image. Headless UI resources use
the same UI_FORMAT and production composition functions. No additional GPU backend,
generic colour-framework dependency, or serialized project migration is introduced.

## 4. Invariants and enforcement

| Invariant | Enforcement |
|---|---|
| Potential never substitutes for current | Distinct newtypes; policy/native-adapter unit tests |
| Updating/closing one output leaves others intact | Destination-addressed commands; state transition tests |
| HDR values survive main-window storage | Shared float format; checked target constructor; GPU pixel proof |
| GPU wrappers cannot lie about drawable storage | Native pixel-format assertion |
| Master FX do not depend on attached displays | SceneLinear mode at the common compositor call site |
| Display mapping uses the existing curves once | Shared WGSL; GPU SDR parity proof |
| Capture RGB is sRGB and alpha remains linear | Typed encoder; numerical, channel-order and metadata tests |
| Invalid headroom/byte lengths fail visibly | Checked constructors and native diagnostics |

## 5. Phasing and acceptance

1. Types, shared GPU mapper, capture conversion and native capability adapter.
   Luna owns independent mechanical scopes; the lead owns decisions and integration.
2. Wire production workspace/output/graph, SDR video/recording and headless paths.
   Remove unaddressed display commands and fixed BGRA8 UI composition.
3. Run focused clippy/tests and production GPU proofs; run the landing gate.
   One bounded UI render checks capture appearance. An instrumented same-display
   app check verifies surface configuration when the environment permits it.
   Actual external SDR/HDR transition behaviour is explicitly unverified without
   that hardware; simulated state tests are not hardware evidence.

Validation: focused clippy with `ui-snapshot,ui-automation` passed. The six initial
production GPU proofs and three policy tests passed through `gpu_proofs_gate.py
--filter presentation`; the added EDR compatibility proof is covered by the
required landing gate. All 12 capture/native-adapter CPU tests passed. One inspector
PNG was rendered and inspected. The isolated native launcher was blocked by the
execution-budget hook even after an exact bounded permit; retries stopped.
Native window and external-display acceptance are tracked in BUG-qdn7.

## 6. Decided — do not reopen

- Preserve linear HDR before independently adapting destinations.
- Keep ordinary UI brightness stable; no automatic artistic/project edits.
- Use native Metal and existing content/UI thread ownership.
- Prefer automated tests and instrumentation; computer use needs a named question.
- Do not claim physical-display verification from a PNG or a simulated capability.

## 7. Deferred

- External mixed-display hardware acceptance: run when displays are connected.
- Universal gamut management and HDR export metadata/matrix audit: separate work.
- Arbitrary N output windows: MULTI_DISPLAY_DESIGN owns that expansion.
- Compile-time typing of every node-graph texture: only required colour/output
  boundaries are typed here; generic shader semantics still require value proofs.
