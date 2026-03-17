# MANIFOLD — C# to Rust Translation Patterns

This document covers the patterns that DON'T have 1:1 equivalents between Unity C# and Rust. These are the places agents improvise and break things. Every pattern here has a SINGLE correct approach — don't invent alternatives.

---

## 1. Inheritance → Trait + Composition

Unity uses deep inheritance chains. Rust has no inheritance. The correct pattern is trait + composition (shared state struct), NOT flattening.

### Unity Pattern
```csharp
public abstract class ShaderGeneratorBase : IGenerator {
    protected Material material;
    protected int width, height;

    public abstract GeneratorType Type { get; }
    protected abstract string ShaderName { get; }

    public virtual void Initialize(int w, int h, Material lineMaterial) { ... }
    public virtual float Render(RenderTexture rt, GeneratorContext ctx) {
        SetStandardUniforms(ctx);
        SetUniforms(ctx);        // override point
        Graphics.Blit(null, rt, material);
        return animProgress;
    }
    protected virtual void SetUniforms(GeneratorContext ctx) { }
}

public class PlasmaGenerator : ShaderGeneratorBase {
    public override GeneratorType Type => GeneratorType.Plasma;
    protected override string ShaderName => "Shaders/GeneratorPlasma";
    protected override void SetUniforms(GeneratorContext ctx) {
        material.SetFloat("_Speed", ctx.Params[0]);
    }
}
```

### Rust Pattern (CORRECT)
```rust
// Shared state — what the base class holds
pub struct ShaderGeneratorState {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group_layout: wgpu::BindGroupLayout,
    pub width: u32,
    pub height: u32,
}

// The Generator trait (from IGenerator)
pub trait Generator {
    fn generator_type(&self) -> GeneratorType;
    fn is_line_based(&self) -> bool { false }
    fn render(&mut self, ..., ctx: &GeneratorContext) -> f32;
    fn resize(&mut self, width: u32, height: u32);
    fn cleanup(&mut self);
}

// Concrete generator owns the base state via composition
pub struct PlasmaGenerator {
    base: ShaderGeneratorState,  // "inherits" via field
    uniform_buffer: wgpu::Buffer,
}

impl Generator for PlasmaGenerator {
    fn generator_type(&self) -> GeneratorType { GeneratorType::Plasma }
    fn render(&mut self, ..., ctx: &GeneratorContext) -> f32 {
        // Same logic as base Render() + SetUniforms() override
        let speed = ctx.param(0, 1.0);
        // ... encode uniforms, dispatch pipeline
    }
}
```

### Rust Pattern (WRONG — do NOT do this)
```rust
// WRONG: Flattening — loses the base class structure
pub fn render_shader_generator(pipeline: &RenderPipeline, ctx: &GeneratorContext) { ... }

// WRONG: Trait with default methods pretending to be a base class
pub trait ShaderGenerator: Generator {
    fn shader_name(&self) -> &str;
    fn set_uniforms(&mut self, ctx: &GeneratorContext) {}
    fn render(&mut self, ...) { /* base impl calling set_uniforms */ }
}

// WRONG: Generic framework
pub struct GenericShaderGenerator<T: ShaderParams> { ... }
```

### Multi-Level Inheritance

For `ShaderGeneratorBase → StatefulShaderGeneratorBase → FluidSimulationGenerator`:

```rust
pub struct FluidSimulationGenerator {
    base: ShaderGeneratorState,       // from ShaderGeneratorBase
    stateful: StatefulState,          // from StatefulShaderGeneratorBase (ping-pong RTs)
    // ...own fields specific to FluidSimulation
    scatter_pipeline: wgpu::ComputePipeline,
    particle_buffer: wgpu::Buffer,
}
```

Each level of the Unity hierarchy becomes a field. Method logic from each level is merged into the concrete impl, preserving the same order of operations.

---

## 2. Mutable Borrowing Across Method Calls

### The Problem
Unity does: `this.compositor.Composite(this.activeClips, this.masterOpacity)` — reading multiple fields of `self` while calling a method on another field of `self`. Rust can't borrow `self` mutably while borrowing its fields.

### Pattern A: Destructure self (preferred for hot paths)
```rust
// Extract fields BEFORE the call
let active_clips = &self.active_clips;
let opacity = self.master_opacity;
self.compositor.composite(active_clips, opacity);
```

### Pattern B: Split borrows via method signature
```rust
// Method takes individual fields, not &mut self
fn composite(
    compositor: &mut Compositor,
    active_clips: &[ActiveClip],
    opacity: f32,
) { ... }
```

### Pattern C: Temporary extraction (for complex cases)
```rust
// Take field out temporarily
let mut compositor = std::mem::take(&mut self.compositor);
compositor.composite(&self.active_clips, self.master_opacity);
self.compositor = compositor;
```

### NEVER do this:
```rust
// WRONG: Arc<Mutex<>> to work around borrows — changes the ownership model
let compositor = Arc::new(Mutex::new(Compositor::new()));

// WRONG: Clone to avoid borrow issues — hides the real problem
let clips_clone = self.active_clips.clone();
self.compositor.composite(&clips_clone, self.master_opacity);

// WRONG: RefCell everywhere — runtime borrow checking defeats the purpose
let compositor = RefCell::new(Compositor::new());
```

---

## 3. Observer Pattern → Rust Equivalents

Unity uses `event Action<T>` for discrete notifications. MANIFOLD's reactivity model is:
- **Continuous state** → polled (playback, compositor, drivers, playhead)
- **Discrete mutations** → evented (clip CRUD, effect changes, selection, BPM)

### Version Counter (for polling — preferred)
```rust
// Unity: UI checks DataVersion each frame
pub struct EditingService {
    data_version: u64,
}

impl EditingService {
    pub fn execute(&mut self, cmd: &mut dyn Command) {
        cmd.execute(&mut self.project);
        self.data_version += 1;  // bump on mutation
    }
    pub fn data_version(&self) -> u64 { self.data_version }
}

// UI side:
if service.data_version() != self.last_seen_version {
    self.refresh();
    self.last_seen_version = service.data_version();
}
```

### Callback Closure (for discrete events)
```rust
// Unity: event Action<TimelineClip> OnClipAdded;
pub struct PlaybackEngine {
    on_clip_added: Option<Box<dyn FnMut(&TimelineClip)>>,
}

impl PlaybackEngine {
    pub fn set_on_clip_added(&mut self, f: impl FnMut(&TimelineClip) + 'static) {
        self.on_clip_added = Some(Box::new(f));
    }
}
```

### NEVER do this:
```rust
// WRONG: Channel for everything — adds async complexity Unity doesn't have
let (tx, rx) = mpsc::channel();

// WRONG: Trait-based observer — over-engineering for single subscriber
trait ClipObserver { fn on_clip_added(&mut self, clip: &TimelineClip); }
```

---

## 4. Dictionary Iteration During Mutation

### Unity Pattern
```csharp
// Pre-allocated removal list (MANIFOLD pattern)
private List<string> removalBuffer = new List<string>();

void CleanupExpired() {
    removalBuffer.Clear();
    foreach (var pair in activeClips) {
        if (pair.Value.IsExpired) removalBuffer.Add(pair.Key);
    }
    foreach (var key in removalBuffer) {
        activeClips.Remove(key);
    }
}
```

### Rust Pattern (CORRECT — mirrors Unity exactly)
```rust
// Pre-allocated removal buffer (same as Unity)
struct PlaybackEngine {
    active_clips: HashMap<String, ActiveClip>,
    removal_buffer: Vec<String>,  // pre-allocated, reused
}

fn cleanup_expired(&mut self) {
    self.removal_buffer.clear();
    for (key, clip) in &self.active_clips {
        if clip.is_expired() {
            self.removal_buffer.push(key.clone());
        }
    }
    for key in &self.removal_buffer {
        self.active_clips.remove(key);
    }
}
```

### Also correct (Rust-specific, but same semantics)
```rust
// retain() is idiomatic Rust and does the same thing
self.active_clips.retain(|_key, clip| !clip.is_expired());
```

`retain()` is fine when Unity's removal logic is simple. If Unity does work during removal (cleanup callbacks, etc.), use the explicit buffer pattern to match the logic flow.

---

## 5. Nullable References → Option<T>

### Mapping Rules

| C# Pattern | Rust Equivalent | Notes |
|---|---|---|
| `SomeClass field = null;` | `field: Option<SomeClass>` | |
| `if (field != null)` | `if let Some(ref f) = self.field` | |
| `field?.Method()` | `self.field.as_ref().map(\|f\| f.method())` | |
| `return null;` | `return None;` | |
| `field = new SomeClass()` | `self.field = Some(SomeClass::new())` | |
| `field ?? defaultValue` | `self.field.unwrap_or(default)` | |
| Method returns null on failure | Method returns `Option<T>` | NOT `Result<T, E>` unless Unity throws |
| Unity throws/crashes | `unwrap()` or `expect("reason")` | Match Unity's crash semantics |

### NEVER do this:
```rust
// WRONG: Result where Unity returns null
fn find_clip(&self, id: &str) -> Result<&TimelineClip, ClipNotFoundError> { ... }
// CORRECT: Option, because Unity returns null
fn find_clip(&self, id: &str) -> Option<&TimelineClip> { ... }

// WRONG: Custom error type for "impossible" states
// CORRECT: expect() with description — matches Unity's crash behavior
let clip = self.clips.get(id).expect("clip must exist in active set");
```

---

## 6. Serde Compatibility with Unity JSON

### Field Naming
Unity's `JsonUtility` serializes C# field names as-is (camelCase). Rust serde defaults to the field name.

```rust
#[derive(Serialize, Deserialize)]
pub struct TimelineClip {
    #[serde(rename = "id")]
    pub id: String,

    #[serde(rename = "startBeat")]
    pub start_beat: f32,

    #[serde(rename = "durationBeats")]
    pub duration_beats: f32,

    #[serde(rename = "inPoint")]
    pub in_point: f32,

    #[serde(rename = "layerIndex")]
    pub layer_index: i32,
}
```

Or use container-level rename:
```rust
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineClip {
    pub id: String,
    pub start_beat: f32,
    pub duration_beats: f32,
    // ...
}
```

### Enum Serialization
Unity serializes enums as integers. Match this:
```rust
#[derive(Serialize, Deserialize, Clone, Copy)]
#[repr(u32)]
pub enum EffectType {
    Transform = 0,
    InvertColors = 1,
    Feedback = 2,
    // ...same numeric values as Unity
}
```

With serde, use `#[serde(try_from = "u32")]` or custom serializer to handle integer ↔ enum.

### Skip / Default
```rust
#[serde(skip)]                    // = [NonSerialized] — runtime-only field
#[serde(default)]                 // = field has default if missing from JSON
#[serde(skip_serializing_if = "Option::is_none")]  // = don't write null fields
```

### Nested Objects
Unity serializes nested objects inline (no `$type` discriminator). Match this:
```rust
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layer {
    pub clips: Vec<TimelineClip>,      // inline array
    pub effects: Vec<EffectInstance>,   // inline array
    pub gen_params: GeneratorParamState, // inline object
}
```

---

## 7. Static / Singleton → Module-Level or Passed Reference

### Unity Pattern
```csharp
public static class EffectDefinitionRegistry {
    private static readonly Dictionary<EffectType, EffectDef> defs = new();
    static EffectDefinitionRegistry() { /* populate */ }
    public static EffectDef Get(EffectType type) => defs[type];
}
```

### Rust Pattern (CORRECT)
```rust
// Module-level functions with lazy_static or once_cell
use std::sync::LazyLock;

static EFFECT_DEFS: LazyLock<HashMap<EffectType, EffectDef>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert(EffectType::InvertColors, EffectDef { ... });
    // ...
    m
});

pub fn get_effect_def(effect_type: EffectType) -> &'static EffectDef {
    EFFECT_DEFS.get(&effect_type).expect("unknown effect type")
}
```

Or simpler — match-based (no HashMap needed for small registries):
```rust
pub fn get_effect_def(effect_type: EffectType) -> EffectDef {
    match effect_type {
        EffectType::InvertColors => EffectDef { name: "Invert Colors", param_count: 0, .. },
        EffectType::Bloom => EffectDef { name: "Bloom", param_count: 4, .. },
        // ...
    }
}
```

---

## 8. IDisposable / Cleanup → Drop or Explicit Cleanup

### Unity Pattern
```csharp
public void Cleanup() {
    if (material != null) Object.Destroy(material);
    if (tempRT != null) { tempRT.Release(); Object.Destroy(tempRT); }
}
```

### Rust Pattern
GPU resources (textures, buffers, pipelines) are dropped automatically when the owning struct goes out of scope. wgpu handles cleanup.

```rust
// Just drop the struct — wgpu cleans up GPU resources
impl Drop for BloomEffect {
    fn drop(&mut self) {
        // wgpu resources cleaned up automatically
        // Only needed if there's non-RAII cleanup (e.g., unregistering from a pool)
    }
}
```

For explicit cleanup matching Unity's `Cleanup()` pattern:
```rust
pub trait PostProcessEffect {
    fn cleanup(&mut self);  // called before drop for ordered cleanup
}
```

---

## 9. Coroutines → Async or Staged Processing

### Unity Pattern
```csharp
IEnumerator ScanLayerFolder(Layer layer) {
    var files = Directory.GetFiles(path, "*.mp4");
    foreach (var file in files) {
        var clip = CreateVideoClip(file);
        layer.AddClip(clip);
        yield return null;  // spread across frames
    }
}
```

### Rust Pattern (frame-staged, not async)
```rust
pub struct FolderScanner {
    pending_files: Vec<PathBuf>,
    current_index: usize,
}

impl FolderScanner {
    pub fn start(path: &Path) -> Self {
        let files: Vec<_> = std::fs::read_dir(path)
            .into_iter().flatten()
            .filter(|e| e.path().extension() == Some("mp4".as_ref()))
            .map(|e| e.path())
            .collect();
        Self { pending_files: files, current_index: 0 }
    }

    /// Process one file per frame. Returns true when done.
    pub fn tick(&mut self, layer: &mut Layer) -> bool {
        if self.current_index >= self.pending_files.len() { return true; }
        let clip = create_video_clip(&self.pending_files[self.current_index]);
        layer.add_clip(clip);
        self.current_index += 1;
        self.current_index >= self.pending_files.len()
    }
}
```

Use actual `async` only for I/O that genuinely benefits from it (network, file system). For frame-spread work, use the staged pattern above — it matches Unity's coroutine semantics more closely.
