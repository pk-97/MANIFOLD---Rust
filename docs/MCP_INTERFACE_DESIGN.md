# MCP Interface — AI Assistant Design

**Status:** Approved design, not implemented. Sonnet-executable.
**Decided:** 2026-07-02. Decided questions in §13 — do not reopen them.

MANIFOLD becomes an MCP server so AI assistants (Claude Desktop, and anything that speaks MCP to a localhost URL) can drive it directly — no Claude Code, no JSON knowledge required from the user.

---

## 1. Goal

Translate thought — abstract ideas, visions — into concrete visuals using traditional rendering techniques. Not AI-generated imagery. The agent writes *patches for a synthesizer*; the synthesizer (the deterministic Metal graph runtime) makes the picture.

This is expected to evolve into the **main interface pattern** for MANIFOLD users. It enhances artists; it does not replace them. The artist stays the judge via the preview feedback loop.

**The anti-slop mechanism is architectural, not aspirational:** the agent can only emit the same JSON graph definitions a human could hand-write. Everything it produces is an ordinary, inspectable, versionable user artifact. No hidden AI-only layer, ever. Future surface growth must preserve this.

**The user experience:** a non-technical user opens Claude Desktop, connects MANIFOLD, types "make me a generator that looks like rain running down glass." The agent searches the catalog, authors a graph, sees rendered frames, iterates, saves a preset into the user's library.

## 2. Non-goals (v1)

- **No remote access.** Localhost only. No ChatGPT-web support (its connector model requires a publicly reachable HTTPS server — that means tunneling the instrument to the internet; explicitly rejected).
- **No timeline arrangement editing.** Timeline is read-only in v1. Graph authoring + params is where AI value is dense; arrangement edits are where risk is dense.
- **No imperative node-by-node editing.** No `add_node`/`wire` verbs. Declarative whole-graph submission only (§6). A `patch_graph` verb for surgical edits to large graphs may come later.
- **No plugin execution.** §10 pins the forward constraint only; the plugin system is its own future design.
- **No multi-client sessions, no OAuth.** One user, one machine, bearer token.

## 3. Architecture

### Crate: `manifold-mcp`

New crate. Dependencies: `manifold-core` (types only), `serde`/`serde_json`, `crossbeam-channel`, the official Rust MCP SDK (`rmcp`) with its streamable-HTTP transport, and `tokio` **isolated to this crate** (current-thread runtime on the MCP thread; tokio must not leak into any other crate). `manifold-app` depends on `manifold-mcp` and wires the channels.

`manifold-mcp` must NOT depend on `manifold-renderer` or `manifold-gpu`. It talks to the running app exclusively through channels.

### Threading

The MCP server runs on its own thread (spawned by `manifold-app` when enabled in settings). It is just another producer of requests to the content thread and consumer of replies — the same lane the UI thread uses. **Zero new shared state** (hard rule).

- **Channel:** dedicated `mcp_tx/mcp_rx` crossbeam channel, bounded 8, MCP thread → content thread. Do not share the UI's `ContentCommand` channel.
- **Request shape:** `McpRequest { kind: McpRequestKind, reply: crossbeam_channel::Sender<McpResponse> }` — each request carries its own bounded(1) reply channel.
- **Content-thread budget:** the content loop services **at most one MCP request per tick**, after `sync_clips_to_time` and rendering. Frame pacing is protected by construction, not by policy. Requests queue in the channel; the MCP thread applies a 10s timeout per request and returns a "busy" error to the client on timeout.
- **Mutations** execute on the content thread through `EditingService` — the same `Command` objects the UI uses. Every AI edit lands on the undo stack; **Cmd-Z works on everything the agent did.** That is the primary safety net.
- **CPU-heavy encode work** (PNG encoding of preview pixels) happens on the MCP thread, never the content thread. The content thread only blits GPU → CPU-visible buffer and hands the bytes over.

## 4. Security model

"Un-hackable" is not a property arbitrary interfaces can have; the design instead caps the worst case at every layer. All of the following are mandatory, not optional hardening:

1. **Bind `127.0.0.1` only.** Never `0.0.0.0`. Not configurable. On a venue LAN, the network attack surface must be zero.
2. **DNS-rebinding defense:** reject any request whose `Host` is not `127.0.0.1`/`localhost` or whose `Origin` header is present and non-localhost. (A malicious webpage can make a browser send requests to localhost ports; this closes it.)
3. **Bearer token.** Generated on first enable, stored in app settings, shown in the settings UI with a copy button. Accepted via `Authorization: Bearer` header, or `?token=` query param for clients that can't set headers. Constant-time comparison. Regenerate button in settings.
4. **No filesystem paths as tool parameters, ever.** `save_preset` writes only into the user preset library directory. Path traversal is removed by design, not by validation.
5. **No code execution verbs.** The surface mutates a project in memory; worst case is a trashed project, and the undo stack covers it. Known asterisk: a graph containing `wgsl_compute` is arbitrary GPU code — Metal sandboxes it; worst case is a GPU hang, not exfiltration. Accepted.
6. **Prompt injection is the residual risk** (an agent reading a malicious webpage can be told to call these tools). It cannot be eliminated; the mitigation is items 4–5 capping what the tools can do at all.
7. **Settings toggle** `allow_structure_edits` (default **on**): gates the mutating/GPU verbs (`preview_graph`, `save_preset`, `transport`). `set_params` and all read verbs are always available. No mode-coupled capability tiers — whether to connect an agent mid-performance is the user's call.

Default port: `7327`, configurable in settings. Server endpoint: `http://127.0.0.1:7327/mcp`.

## 5. Tool surface (v1 — 14 tools)

Read verbs execute against the content thread's `Project` (via the request channel; the content thread serializes the answer). Compact JSON everywhere; token cost is UX for agents.

| Tool | Params | Returns |
|---|---|---|
| `get_project_overview` | — | project name, tempo, transport state, layer tree (id, name, type, clip count), scene list, `allow_structure_edits` state |
| `list_nodes` | `filter?` (substring on name/purpose), `category?` | compact array: `type_id`, name, category, one-line purpose, port summary |
| `get_node_docs` | `type_ids: [string]` | full spec per node: ports (name, channel type, required, default), params (name, type, range, default), purpose, usage notes |
| `list_presets` | `kind?: "effect" \| "generator"` | bundled + user presets: id, name, description, node count |
| `get_graph` | `preset_id` | the full graph-definition JSON |
| `validate_graph` | `graph_json`, `kind` | `{ valid, errors: [{node_id, port?, message}], warnings }` |
| `preview_graph` | `graph_json`, `kind`, `beats: [f64]`, `width?`, `height?`, `input?` | PNG image content per beat (MCP image results) |
| `save_preset` | `graph_json`, `kind`, `name`, `description?` | preset id (user library only) |
| `set_params` | `target` (effect instance / generator clip), `values: {param_id: value}` | ok / per-param errors |
| `get_output_snapshot` | `width?` | PNG of the current program output |
| `transport` | `action: "play" \| "stop" \| "seek"`, `beat?` | ok |
| `get_mood_board` | — | all entries: notes as text, links as URLs, images/videos as 256px thumbnails + captions + tags (§8) |
| `get_mood_reference` | `entry_id`, `max_dim?` | image at up to 1024px, or multi-frame contact sheet for a video entry |
| `add_mood_note` | `text`, `tags?` | entry id (agent-authored note, undoable) |

Gated by `allow_structure_edits`: `preview_graph`, `save_preset`, `transport`. Everything else always on.

**Surface growth rule:** grow by nouns (session grid, timeline, displays), not by per-feature tool piles. The surface must stay small enough for an agent to hold entirely in context.

**Amendment (2026-07-02):** the component tier (`docs/COMPONENT_LIBRARY_DESIGN.md`) extends this surface — `list_nodes` gains `tier: "atom" | "component"`, `get_node_docs` serves component interfaces, and `save_component` joins the gated verbs. Agents compose components first, atoms to glue, raw WGSL last.

### Catalog = the live registry. Single source of truth.

`list_nodes` / `get_node_docs` serve **the primitive registry directly** — the same data that drives the app. Never a separately maintained AI-facing document: doc drift silently degrades every agent's output quality, and if AI is the main interface, the vocabulary *is* the product.

- The node-descriptor backend (friendly names, taxonomy, roles, aliases, tooltips — see `project_node_descriptor_ux_backend`) is the payload of `get_node_docs`. Work invested there directly improves agent authoring.
- `docs/NODE_CATALOG.md` remains the human-facing doc; both derive from the registry.
- Bundled presets (via `list_presets` + `get_graph`) are the few-shot examples — an agent reading 2–3 reference presets before authoring is the expected pattern, same as the §2.5 audit rule for humans.

## 6. Authoring loop (declarative, not imperative)

Presets already ARE JSON graph definitions. The agent authors the same artifact the 45 bundled presets are made of:

```
list_nodes / get_node_docs / get_graph (references)
        ↓
validate_graph(json)   →  errors as text, no GPU, fast
        ↓ (iterate until valid)
preview_graph(json, beats: [0, 2, 8])  →  rendered PNGs
        ↓ (iterate until it looks right — the artist judges)
save_preset(json, name)
```

Why declarative: atomic, no stale imperative state held by the model, validation errors reference the submitted document directly, and the artifact is inspectable at every step.

`validate_graph` runs the existing graph compile path **minus GPU dispatch**: unknown primitive types, channel-type mismatches on wires, unwired required ports, cycle check honoring per-port state capture (`state_capture_input_ports`), param name/range validation. Error messages must name the node id and port — they are consumed by a model, and vague errors burn the user's tokens.

## 7. Preview render path

- Executes **on the content thread** (it owns the pipeline and GPU submission), as the one-per-tick serviced request. Renders the submitted graph offscreen at each requested beat, blits to a CPU-visible buffer; PNG encode happens on the MCP thread.
- **Multiple beats per call** (contact sheet) so the agent sees *motion*, not one frame. Without temporal feedback the model authors blind — the parity migrations proved how wrong blind-authored shaders are.
- Caps: max 8 beats per call, max 1024px on the long edge, default 512. Stateful graphs (feedback loops, sims) warm up by running from beat 0 to the requested beat at project FPS, capped at 16 beats of warmup; document the cap in the tool description so the agent knows.
- **Inputs:** generators need none. Effects receive `input`: `"test_pattern"` (default — SMPTE-style bars plus a moving gradient so temporal behavior is visible) or `{clip_id}` — a frame of the user's own footage from the project. Previewing against the artist's own material is what makes output theirs instead of generic; test-pattern-only ships in P3, clip input in P5.
- `get_output_snapshot` is the cheap live variant: content thread blits the IOSurface front buffer, MCP thread encodes. Read-only, perform-safe.

## 8. Mood boards

The user curates a per-project mood board — reference images, text notes, tags ("Blade Runner 2049 titles", "brutalist", "slow, liturgical") — and the agent reads it as the brief before authoring. The agent is multimodal: it *sees* the references and translates palette, motion character, and composition into graph choices.

Data model (`manifold-core`, serialized on `Project` — same serde pattern as the session grid):

```rust
pub struct MoodBoard { pub entries: Vec<MoodEntry> }  // #[serde(default, skip_serializing_if = "MoodBoard::is_empty")]

pub struct MoodEntry {
    pub id: MoodEntryId,
    pub kind: MoodEntryKind,
    pub caption: String,
    pub tags: Vec<String>,
    pub agent_authored: bool,  // agent notes are visibly marked in the UI
}

pub enum MoodEntryKind {
    Image { path: String },  // dropped or pasted
    Video { path: String },  // local file; motion reference
    Link { url: String },    // web reference — inert string to MANIFOLD
    Note { text: String },
}
```

- **Images and videos are path references, like all other project media** (video clips). No new asset-embedding mechanism — the V2 ZIP stores project JSON snapshots, not media; portability is the same problem video clips already have and gets solved once for all media if ever.
- **Paste support:** pasteboard images have no path, so the app writes the bytes to a managed directory (`~/Library/Application Support/MANIFOLD/moodboard/<content-hash>.png`) and stores that path. The data model stays paths-only.
- **Video entries** show a poster-frame thumbnail on the board; `get_mood_reference` on a video returns a multi-frame contact sheet (existing `manifold-media` decode path, same frame-grab used for clip thumbnails) so the agent reads *motion character*, not one frame. Caps: max 8 frames, 1024px long edge.
- **Link entries are inert.** `get_mood_board` returns the URL + caption; the agent's own web tools fetch it if its client allows. **MANIFOLD never makes outbound network requests** — no thumbnail scraping, no oEmbed. This is a security posture, not a missing feature: the MCP layer keeps zero outbound network surface.
- Board edits go through `EditingService` like everything else — undoable. UI is a simple panel (drag-drop images, type notes); UI detail is out of scope for this doc.
- `add_mood_note` is how agreed direction from a conversation gets captured into the project ("settled: monochrome, heavy grain, pulse on the kick") — the next session's agent reads the board and continues from the same brief. It is a shared memory between user, agent, and future agents. Ungated by `allow_structure_edits` (a note, not structure).

**Anti-slop guard: mood boards are inspiration input only.** Reference images are never fed into rendering as source material — no img2img, no style-transfer path from board to output. The agent looks, then authors.

## 9. What does NOT change

- `sync_clips_to_time()` remains the sole playback authority; `transport` sends the same commands the UI does.
- `EditingService` remains the sole mutation gateway. No MCP-special mutation path.
- Two-thread model unchanged; the MCP thread is a third *requester*, not a third owner.
- The renderer, graph runtime, and preset formats are untouched. The MCP layer is adapters + transport only.

## 10. Forward constraint: user plugins (future, pinned now)

Peter intends user-authored input/output plugins (custom MIDI processors, network protocols, LED mappers), authored largely through this interface. Pinned decisions so the MCP surface doesn't foreclose them:

- **Substrate: WASM sandbox** (wasmtime) with **capability manifests**. A plugin declares what it needs ("receive MIDI, emit control values"; "open UDP to host X"); the host exposes only those imports. The boundary is the substrate, not a policy check. Control-rate work fits WASM; pixel-rate work stays in graphs/WGSL.
- **Native FFI stays the pro escape hatch** (like `manifold-native`), explicitly gated behind "this can do anything to your machine" consent. Not the ecosystem path.
- **Agents author; only humans grant.** A future `submit_plugin` tool returns `"pending user approval"` — capability grants happen in the MANIFOLD UI, click-by-a-human, never via a tool call. A prompt-injected agent can write a malicious plugin but cannot give it hands.

## 11. Phasing

Each phase gates on its test before the next starts.

- **P1 — Server skeleton.** `manifold-mcp` crate, thread + channel wiring, token auth, Host/Origin checks, settings (enable toggle, port, token display/regenerate, `allow_structure_edits`). Tools: `get_project_overview`, `list_nodes`, `get_node_docs`, `list_presets`. *Gate:* connect from Claude Desktop, query the catalog; requests with bad token / bad Origin rejected (unit-tested).
- **P2 — Read + validate.** `get_graph`, `validate_graph`. *Gate:* every bundled preset round-trips `get_graph` → `validate_graph` clean; deliberately broken graphs (bad channel type, missing required port, unknown primitive, cycle) produce errors naming node + port.
- **P3 — Preview.** `preview_graph` (test-pattern input), offscreen path, PNG plumbing. *Gate:* contact sheet of 3 bundled generators + 3 bundled effects is visually correct; a 60fps playback session shows no missed frames while previews are being serviced (one-per-tick budget verified).
- **P4 — Write.** `save_preset`, `set_params`, `get_output_snapshot`, `transport`. *Gate:* full loop from Claude Desktop — author a novel generator cold, iterate via previews, save, load it in the app, undo an agent `set_params` with Cmd-Z.
- **P5 — Mood boards + polish.** `MoodBoard` data model + panel, the three mood tools, `input: {clip_id}` previews, error-message quality pass driven by the vocabulary stress-test gap report, docs. *Gate:* given a 3-image board, an agent authors a preset that visibly matches the board's palette without being told the colors.

## 12. Relationship to the vocabulary stress-test

The stress-test (Fable authors presets cold using only what `list_nodes`/`get_node_docs` would serve) is the acceptance test for §5's catalog design and is **critical path** — if the main interface is AI, agent expressive range equals catalog quality. Its gap report feeds the descriptor backend and P5.

## 13. Decided questions — do not reopen

1. Declarative graph submission, not imperative node-by-node editing.
2. Embedded server in the running app (streamable HTTP, localhost), not a stdio sidecar — the point is the *running* instance.
3. Localhost only; no ChatGPT-web tunnel; remote access only ever as a future explicit opt-in.
4. Bearer token + Host/Origin validation + 127.0.0.1 binding: all mandatory.
5. No filesystem paths as tool parameters; `save_preset` targets the library dir only.
6. Timeline arrangement read-only in v1.
7. No mode-coupled capability tiers; single `allow_structure_edits` toggle, default on. Connecting an agent mid-set is the user's call.
8. Catalog served from the live registry; no separately maintained AI doc.
9. AI output = ordinary user artifacts (plain JSON presets); no hidden AI layer. Anti-slop is architectural.
10. Plugin substrate = WASM + capability manifests; native FFI = gated escape hatch; agents author, humans grant.
11. Content thread services at most one MCP request per tick; PNG encode off the content thread.
12. `manifold-mcp` crate isolates tokio; depends on `manifold-core` only among workspace crates.
13. Mood boards are inspiration-only context: reference images/videos never enter the render path (no img2img, no style transfer). Media entries are path references like all other project media; pasted images are written to a managed dir first. Link entries are inert URLs — MANIFOLD never makes outbound network requests.
