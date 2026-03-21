# Interface → Trait and Base Class → Pattern Map

Reference for Unity-to-Rust architectural mapping. These tables document the structural translation contracts.

## Key Interfaces → Rust Traits

| Unity Interface | Rust Trait | Crate | Key Methods |
|---|---|---|---|
| `IClipRenderer` | `ClipRenderer` | `manifold-playback` | `can_handle`, `start_clip`, `stop_clip`, `get_texture`, `is_clip_ready`, `pre_render`, `resize`, `release_all` |
| `IPostProcessEffect` | `PostProcessEffect` | `manifold-renderer` | `effect_type`, `initialize`, `apply`, `clear_state`, `resize`, `cleanup`, `cleanup_owner_state` |
| `IStatefulEffect` | `StatefulEffect: PostProcessEffect` | `manifold-renderer` | `clear_state_for_owner`, `cleanup_owner`, `cleanup_all_owners` |
| `IGenerator` | `Generator` | `manifold-renderer` | `generator_type`, `is_line_based`, `initialize`, `render`, `resize`, `cleanup` |
| `IEffectContainer` | `EffectContainer` | `manifold-core` | `effects()`, `effect_groups()`, `has_modular_effects()`, `envelopes()` |
| `IParamSource` | `ParamSource` | `manifold-core` | `param_count`, `get_param`, `set_param`, `get_base_param`, `set_base_param`, `get_param_def` |
| `ISyncSource` | `SyncSource` | `manifold-playback` | `is_enabled`, `display_name`, `enable`, `disable`, `toggle` |
| `ILiveClipHost` | `LiveClipHost` | `manifold-playback` | `current_project`, `current_time`, `current_beat`, `is_recording`, `mark_sync_dirty`, `record_command` |
| `ICommand` | `Command` | `manifold-editing` | `description`, `execute`, `undo` |
| `IInspectorPanel` | `InspectorPanel` | `manifold-ui` | `is_active`, `panel_height`, `show_empty`, `refresh_after_undo`, `sync_from_data` |
| `IExternalOutput` | `ExternalOutput` | (future) | `initialize`, `process_frame`, `blackout`, `shutdown` |

## Base Classes → Trait + Shared State

| Unity Base Class | Rust Pattern | Key Responsibilities |
|---|---|---|
| `ShaderGeneratorBase` | `trait ShaderGenerator` + helper fns | Material lifecycle, standard uniforms (_Time2, _Beat, _AspectRatio, etc.) |
| `LineGeneratorBase` | `trait LineGenerator` + line mesh utils | Line drawing with LineMeshUtil |
| `StatefulShaderGeneratorBase` | Extends `ShaderGenerator` + `StatefulState` struct | Ping-pong RT pair for temporal feedback |
| `ComputeVolumeGeneratorBase` | `trait VolumeGenerator` + volume state | 3D compute + raymarch display |
| `ComputeParticleGeneratorBase` | `trait ParticleGenerator` + particle state | Compute particles + scatter splat |
| `SimpleBlitEffect` | `trait SimpleBlitEffect` + blit helper | Single-pass shader blit for effects |

## C# → Rust Type Mapping

| Unity (C#) | Rust | Notes |
|---|---|---|
| `interface IFoo` | `trait Foo` | Same methods, same signatures, same contract |
| `abstract class FooBase` | `trait FooBase` + shared state struct | Preserve hierarchy as trait + composition |
| `class Foo : FooBase` | `struct Foo` + `impl FooBase for Foo` | Same fields, same method bodies |
| `class FooService` | `struct FooService` | Same public API surface, same internal logic |
| `class FooController : MonoBehaviour` | `struct FooController` with lifecycle | `Awake()` → `new()`, `Update()` → `tick()`, `OnDestroy()` → `Drop` |
| `enum FooType` | `enum FooType` | Same variants, same numeric values |
| HLSL shader | WGSL shader | Same math, same variable names, same order |
| `List<T>` | `Vec<T>` | |
| `Dictionary<K,V>` | `AHashMap<K,V>` (hot path) or `HashMap<K,V>` | |
| `HashSet<T>` | `HashSet<T>` or `AHashSet<T>` | |
| `event Action<T>` | callback / channel | Same firing semantics |
| `[NonSerialized]` | `#[serde(skip)]` | |
| `[SerializeField]` | `#[serde(rename = "...")]` | |
| nullable reference | `Option<T>` | |
