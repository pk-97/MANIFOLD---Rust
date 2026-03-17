# MANIFOLD — Unity API → Rust Mapping

Every Unity runtime API used in MANIFOLD's core logic (playback, compositing, editing, sync, data) and its exact Rust equivalent. Agents: when you encounter a Unity API call during porting, look it up here first.

---

## Time

Unity time APIs are replaced by values provided from the app's frame loop. No global time queries.

| Unity API | Rust Equivalent | Where Provided |
|---|---|---|
| `Time.deltaTime` | `ctx.dt` or `delta_time` parameter | `GeneratorContext.dt`, `TickContext.delta_time` |
| `Time.unscaledDeltaTime` | `Instant::elapsed()` from frame start | App loop computes real elapsed time |
| `Time.realtimeSinceStartup` | `Instant::now() - app_start` | Store `Instant` at app init |
| `Time.time` | `ctx.time` | `GeneratorContext.time`, `EffectContext.time` |
| `Time.frameCount` | `self.frame_count: u64` | Increment per render frame in app state |
| `Time.captureFramerate` | `self.export_fixed_delta: Option<f32>` | When `Some`, use fixed delta instead of real time |
| `Application.targetFrameRate` | `winit` present mode / vsync | Set via window surface configuration |

**Rule:** Generators and effects NEVER query time directly. They receive it via context structs. Match Unity's `GeneratorContext` and `EffectContext` exactly.

---

## Math

All `Mathf` methods map to Rust `f32` methods. Use `glam` for vector/matrix types.

| Unity API | Rust Equivalent | CRITICAL NOTES |
|---|---|---|
| `Mathf.Abs(x)` | `x.abs()` | |
| `Mathf.Sign(x)` | `if x >= 0.0 { 1.0 } else { -1.0 }` | Unity returns 1.0 for 0, NOT 0.0 |
| `Mathf.Clamp(x, min, max)` | `x.clamp(min, max)` | |
| `Mathf.Clamp01(x)` | `x.clamp(0.0, 1.0)` | |
| `Mathf.Lerp(a, b, t)` | `a + (b - a) * t.clamp(0.0, 1.0)` | **Lerp CLAMPS t to [0,1]** |
| `Mathf.LerpUnclamped(a, b, t)` | `a + (b - a) * t` | No clamping |
| `Mathf.InverseLerp(a, b, v)` | `((v - a) / (b - a)).clamp(0.0, 1.0)` | Clamped output |
| `Mathf.SmoothStep(a, b, t)` | `let t = t.clamp(0.0, 1.0); let t = t*t*(3.0-2.0*t); a + (b-a)*t` | Hermite |
| `Mathf.MoveTowards(cur, target, max_delta)` | see below | |
| `Mathf.Repeat(t, length)` | `t - (t / length).floor() * length` | NOT `t % length` (sign differs for negatives) |
| `Mathf.PingPong(t, length)` | `length - (Repeat(t, length * 2.0) - length).abs()` | |
| `Mathf.RoundToInt(x)` | `x.round() as i32` | NOT `x as i32` (that truncates!) |
| `Mathf.FloorToInt(x)` | `x.floor() as i32` | |
| `Mathf.CeilToInt(x)` | `x.ceil() as i32` | |
| `Mathf.Max(a, b)` | `a.max(b)` or `f32::max(a, b)` | |
| `Mathf.Min(a, b)` | `a.min(b)` or `f32::min(a, b)` | |
| `Mathf.Pow(base, exp)` | `base.powf(exp)` | |
| `Mathf.Sqrt(x)` | `x.sqrt()` | |
| `Mathf.Log(x)` | `x.ln()` | Natural log |
| `Mathf.Log10(x)` | `x.log10()` | |
| `Mathf.Exp(x)` | `x.exp()` | |
| `Mathf.Sin(x)` | `x.sin()` | |
| `Mathf.Cos(x)` | `x.cos()` | |
| `Mathf.Atan2(y, x)` | `y.atan2(x)` | |
| `Mathf.PI` | `std::f32::consts::PI` | |
| `Mathf.Infinity` | `f32::INFINITY` | |
| `Mathf.Epsilon` | `f32::EPSILON` | |

```rust
// MoveTowards — not a built-in, implement explicitly
fn move_towards(current: f32, target: f32, max_delta: f32) -> f32 {
    if (target - current).abs() <= max_delta {
        target
    } else {
        current + (target - current).signum() * max_delta
    }
}
```

### Vector Types

| Unity | Rust (glam) |
|---|---|
| `Vector2(x, y)` | `glam::Vec2::new(x, y)` |
| `Vector3(x, y, z)` | `glam::Vec3::new(x, y, z)` |
| `Vector4(x, y, z, w)` | `glam::Vec4::new(x, y, z, w)` |
| `Vector3.zero` | `Vec3::ZERO` |
| `Vector3.one` | `Vec3::ONE` |
| `Color(r, g, b, a)` | `[f32; 4]` or `glam::Vec4` |
| `Matrix4x4` | `glam::Mat4` |

---

## Texture / RenderTarget

| Unity API | Rust Equivalent |
|---|---|
| `new RenderTexture(w, h, 0, format)` | `device.create_texture(TextureDescriptor { size: Extent3d { width: w, height: h, .. }, format, usage: RENDER_ATTACHMENT \| TEXTURE_BINDING \| ... })` |
| `rt.Create()` | Implicit on `create_texture()` |
| `rt.Release()` | Drop the `wgpu::Texture` (RAII) |
| `RenderTexture.active = rt` | Begin a `wgpu::RenderPass` targeting the texture view |
| `Graphics.Blit(src, dst, material)` | Draw fullscreen triangle with pipeline bound to dst, src as sampled texture |
| `Graphics.Blit(src, dst, material, pass)` | Select pipeline for specific pass, then draw |
| `Graphics.CopyTexture(src, dst)` | `encoder.copy_texture_to_texture(src, dst, size)` |
| `GL.Clear(true, true, Color.clear)` | `RenderPass` with `LoadOp::Clear { color: ... }` |
| `Texture2D.blackTexture` | Create static 1x1 black texture at init |
| `texture.filterMode = FilterMode.Bilinear` | `SamplerDescriptor { mag_filter: Linear, min_filter: Linear, .. }` |
| `texture.wrapMode = TextureWrapMode.Clamp` | `SamplerDescriptor { address_mode_u: ClampToEdge, .. }` |
| `texture.GetNativeTexturePtr()` | `texture.as_hal::<Metal>(...)` for platform interop |

### Texture Format Mapping (EXACT — no substitutions)

| Unity Format | wgpu Format | Usage |
|---|---|---|
| `RFloat` | `R32Float` | Single-channel 32-bit (density, masks) |
| `RGFloat` | `Rg32Float` | Two-channel 32-bit (vector fields) |
| `RHalf` | `R16Float` | Single-channel 16-bit |
| `RGHalf` | `Rg16Float` | Two-channel 16-bit |
| `ARGBHalf` | `Rgba16Float` | Four-channel 16-bit (HDR render targets) |
| `ARGBFloat` | `Rgba32Float` | Four-channel 32-bit (high-precision state) |
| `ARGB32` | `Rgba8Unorm` | Four-channel 8-bit (LDR, thumbnails) |
| `BGRA32` | `Bgra8Unorm` | Surface/swapchain format |
| `RGB565` | `R5g6b5Unorm` (if available) | Compressed format |

---

## Compute Shader

| Unity API | Rust Equivalent |
|---|---|
| `ComputeShader.FindKernel(name)` | One `ComputePipeline` per kernel, created at init |
| `shader.SetFloat(kernel, "_Name", val)` | Write to uniform buffer struct field, `queue.write_buffer()` |
| `shader.SetVector(kernel, "_Name", vec4)` | Write `[f32; 4]` to uniform buffer struct |
| `shader.SetInt(kernel, "_Name", val)` | Write `u32` or `i32` to uniform buffer struct |
| `shader.SetBuffer(kernel, "_Name", buf)` | Bind via `BindGroup` with `BufferBinding` |
| `shader.SetTexture(kernel, "_Name", tex)` | Bind via `BindGroup` with `TextureView` (storage or sampled) |
| `shader.Dispatch(kernel, x, y, z)` | `compute_pass.dispatch_workgroups(x, y, z)` |
| `new ComputeBuffer(count, stride)` | `device.create_buffer(BufferDescriptor { size: count * stride, usage: STORAGE, .. })` |
| `buffer.SetData(array)` | `queue.write_buffer(buffer, 0, bytemuck::cast_slice(&data))` |
| `buffer.Release()` | Drop `wgpu::Buffer` (RAII) |

**Pattern:** All `SetFloat/SetVector/SetInt` calls for a single dispatch → one uniform buffer struct. Encode all values, write once, bind once.

```rust
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct FluidUniforms {
    dt: f32,
    viscosity: f32,
    curl_strength: f32,
    resolution: u32,
    // ... pad to 16-byte alignment
    _pad: [f32; 0],  // adjust as needed
}
```

---

## Material / Shader

| Unity API | Rust Equivalent |
|---|---|
| `new Material(shader)` | Create `RenderPipeline` + `BindGroupLayout` from compiled shader |
| `material.SetFloat("_Name", val)` | Write to uniform buffer, same as compute |
| `material.SetVector("_Name", vec4)` | Write to uniform buffer |
| `material.SetTexture("_Name", tex)` | Create `BindGroup` with texture view + sampler |
| `material.SetColor("_Name", color)` | Write `[f32; 4]` to uniform buffer |
| `material.SetInt("_Name", val)` | Write `u32` to uniform buffer |
| `Shader.PropertyToID("_Name")` | Not needed — uniforms are struct offsets, textures are bind group entries |

**The SetUniforms() → uniform buffer pattern:**
```rust
// Unity:
// material.SetFloat("_Time2", time);
// material.SetFloat("_Beat", beat);
// material.SetFloat("_AspectRatio", aspect);

// Rust:
#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct GeneratorUniforms {
    time: f32,
    beat: f32,
    aspect_ratio: f32,
    _pad: f32,
}

queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
```

---

## Serialization

| Unity API | Rust Equivalent |
|---|---|
| `[Serializable]` | `#[derive(Serialize, Deserialize)]` |
| `[SerializeField]` | Public field (serde serializes public by default) |
| `[NonSerialized]` | `#[serde(skip)]` |
| `[JsonIgnore]` | `#[serde(skip)]` |
| `JsonUtility.FromJson<T>(json)` | `serde_json::from_str::<T>(&json)` |
| `JsonUtility.ToJson(obj)` | `serde_json::to_string(&obj)` |
| `JsonUtility.ToJson(obj, true)` | `serde_json::to_string_pretty(&obj)` |

**Enum serialization:** Unity serializes enums as integers. Use:
```rust
// In types.rs, enums use integer representation
impl Serialize for EffectType {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error> {
        (*self as u32).serialize(s)
    }
}
```

---

## Persistence

| Unity API | Rust Equivalent | Notes |
|---|---|---|
| `Application.persistentDataPath` | `dirs::config_dir().join("MANIFOLD")` | macOS: `~/Library/Application Support/MANIFOLD` |
| `Application.dataPath` | Not needed at runtime | |
| `PlayerPrefs.GetString(key, default)` | `UserPrefs::get_string(key, default)` | Custom JSON file in config dir |
| `PlayerPrefs.SetString(key, value)` | `UserPrefs::set_string(key, value)` | |
| `PlayerPrefs.Save()` | `UserPrefs::save()` | Write JSON to disk |

---

## VideoPlayer

Not yet ported. Will require FFmpeg or platform decoder. The trait interface:

| Unity API | Rust Trait Method |
|---|---|
| `player.Play()` | `fn play(&mut self)` |
| `player.Pause()` | `fn pause(&mut self)` |
| `player.Stop()` | `fn stop(&mut self)` |
| `player.Prepare()` | `fn prepare(&mut self)` |
| `player.isPrepared` | `fn is_prepared(&self) -> bool` |
| `player.time` (get) | `fn current_time(&self) -> f64` |
| `player.time` (set) | `fn seek(&mut self, seconds: f64)` |
| `player.playbackSpeed` | `fn set_playback_speed(&mut self, speed: f32)` |
| `player.isLooping` | `fn set_looping(&mut self, looping: bool)` |
| `player.url` | `fn set_source(&mut self, path: &Path)` |
| `player.targetTexture` | `fn output_texture(&self) -> &wgpu::TextureView` |
| `player.skipOnDrop = true` | `fn set_frame_drop_policy(&mut self, drop: bool)` |

---

## Async GPU Readback

| Unity API | Rust Equivalent |
|---|---|
| `AsyncGPUReadback.Request(texture, callback)` | Copy texture → staging buffer, then `buffer.slice(..).map_async()` |
| `request.hasError` | Check `Result` from map callback |
| `request.GetData<byte>()` | `buffer.slice(..).get_mapped_range()` |

```rust
// Pattern: copy texture to staging buffer, poll for completion
encoder.copy_texture_to_buffer(
    texture.as_image_copy(),
    wgpu::ImageCopyBuffer {
        buffer: &staging_buffer,
        layout: wgpu::ImageDataLayout { ... },
    },
    texture_size,
);
// Submit, then poll:
staging_buffer.slice(..).map_async(wgpu::MapMode::Read, |result| { ... });
device.poll(wgpu::Maintain::Wait);
```

---

## Lifecycle (MonoBehaviour → Explicit)

| Unity Lifecycle | Rust Equivalent | When Called |
|---|---|---|
| `Awake()` | `::new()` or `::init()` | Construction / first init |
| `Start()` | First frame of app loop | After all systems initialized |
| `OnEnable()` | Explicit `enable()` method | When system activated |
| `Update()` | `tick(&mut self, ctx: &FrameContext)` | Every frame, main logic |
| `LateUpdate()` | `post_frame(&mut self)` | After all `tick()` calls complete |
| `OnDisable()` | Explicit `disable()` method | When system deactivated |
| `OnDestroy()` | `Drop` impl or explicit `cleanup()` | Shutdown |

---

## Native / Platform

| Unity API | Rust Equivalent |
|---|---|
| `[DllImport("libc")] popen()` | `std::process::Command::new(...)` |
| `[DllImport("MetalEncoder")]` | `objc` / `metal-rs` crate for Metal framework |
| `Texture2D.CreateExternalTexture()` | Platform-specific texture sharing via `wgpu::hal` |
| `Application.platform` | `cfg!(target_os = "macos")` / `std::env::consts::OS` |

---

## Resources / Asset Loading

| Unity API | Rust Equivalent |
|---|---|
| `Resources.Load<Shader>(path)` | `include_str!("shaders/name.wgsl")` for embedded, or `std::fs::read_to_string()` |
| `ComputeShaderCache.Load(name)` | Cache compiled `ShaderModule` in HashMap, create once at init |

**Pattern:** Embed shaders at compile time with `include_str!`. No runtime file loading for shaders.
