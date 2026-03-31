# Fix: Vertex Buffer Corruption (Triangle Spike Artifacts)

Read `CLAUDE.md` before starting.

## Problem

The UI renders triangle spike artifacts during interaction (clicking, hovering, navigating). This is vertex buffer corruption caused by shared GpuBuffer aliasing — the CPU overwrites vertex data via mapped pointers while the GPU is still reading from a previously committed (but not yet executed) command buffer.

The root cause: Phase 5 converted UIRenderer from creating fresh wgpu buffers each frame (`create_buffer_init`) to reusing pre-allocated shared GpuBuffers. On Apple Silicon unified memory, shared buffers are the same physical memory for CPU and GPU. `encoder.commit()` queues GPU work but does NOT wait for completion. If `prepare()` overwrites the shared buffer before the GPU finishes a previous draw, the GPU reads corrupted vertex data → triangle spikes.

## Fix Strategy

**Create fresh shared GpuBuffers each `prepare()` call.** This matches what the wgpu version did (new buffer per frame). The Metal allocator on Apple Silicon is fast — small buffer allocations are sub-microsecond. The "zero per-frame allocation" optimization was premature and introduced the aliasing bug.

Do NOT try to fix this with fences, semaphores, or ring buffers. Fresh buffers per prepare is the simplest correct solution.

## Files to Fix

Read each file fully before making changes.

### 1. `crates/manifold-renderer/src/ui_renderer.rs`

**In the struct:** Remove pre-allocated buffer fields (`vertex_buf`, `index_buf`, `vertex_capacity`, `index_capacity`). Replace with:
```rust
prepared_vertex_buf: Option<GpuBuffer>,
prepared_index_buf: Option<GpuBuffer>,
```

**In `new()`:** Remove the pre-allocated `create_buffer_shared()` calls for vertex/index buffers.

**In `prepare()` / `prepare_with_offset()`:** After building `self.vertices` and `self.indices` from rect commands, create fresh buffers sized to the actual data:
```rust
if !self.vertices.is_empty() {
    let vbuf_size = (self.vertices.len() * std::mem::size_of::<UIVertex>()) as u64;
    let vbuf = device.create_buffer_shared(vbuf_size);
    unsafe {
        std::ptr::copy_nonoverlapping(
            self.vertices.as_ptr() as *const u8,
            vbuf.mapped_ptr().unwrap() as *mut u8,
            vbuf_size as usize,
        );
    }

    let ibuf_size = (self.indices.len() * std::mem::size_of::<u32>()) as u64;
    let ibuf = device.create_buffer_shared(ibuf_size);
    unsafe {
        std::ptr::copy_nonoverlapping(
            self.indices.as_ptr() as *const u8,
            ibuf.mapped_ptr().unwrap() as *mut u8,
            ibuf_size as usize,
        );
    }

    self.prepared_vertex_buf = Some(vbuf);
    self.prepared_index_buf = Some(ibuf);
    self.prepared_index_count = self.indices.len() as u32;
} else {
    self.prepared_vertex_buf = None;
    self.prepared_index_buf = None;
    self.prepared_index_count = 0;
}
```

**In `render()`:** Use `self.prepared_vertex_buf.as_ref().unwrap()` and `self.prepared_index_buf.as_ref().unwrap()` for the draw_indexed call.

**Check `mapped_ptr()`:** Verify whether `mapped_ptr` is a method (call with `()`) or a field (access directly). The Phase 3 notes say it's a method — check the actual GpuBuffer type in manifold-gpu.

### 2. `crates/manifold-renderer/src/native_text.rs`

Same pattern. Find the vertex/index buffer handling in NativeTextRenderer. If it reuses pre-allocated shared buffers across prepare() calls, apply the same fix: create fresh buffers each prepare() sized to the actual data.

Look for fields like `vertex_buf: GpuBuffer` and `index_buf: GpuBuffer` on the struct. Replace with `Option<GpuBuffer>` and create fresh each prepare().

### 3. `crates/manifold-renderer/src/layer_bitmap_gpu.rs`

Check the per-layer `vertex_bufs: Vec<Option<GpuBuffer>>`. These are shared buffers written each `render_layers()` call. The same aliasing risk exists if render_layers is called multiple times per frame with different data.

However, LayerBitmapGpu's vertex buffers are small (64 bytes per layer) and each layer's buffer is only written once per frame, so this may not be the active bug. Still, audit it:

- If a layer's vertex data changes between frames, the old buffer data must not be in-flight when overwritten.
- The safest fix: create fresh vertex buffers in `render_layers()` for each layer that has data, same as UIRenderer.
- Or: if the current approach works without artifacts for layer bitmaps specifically, leave it.

### 4. `crates/manifold-app/src/app_render.rs`

No structural changes needed. But verify that:
- UICacheManager's `render_dirty_panels()` creates and commits its own encoders (each panel gets its own encoder → commit). This is already the case from Phase 5.
- The main frame encoder is separate from the panel cache encoders.
- UIRenderer.prepare() is called ONCE for the overlay pass (not reused from a panel cache prepare).

## Verification

1. `cargo clippy --workspace -- -D warnings` — must pass
2. `cargo test --workspace` — must pass
3. Launch the app. Navigate the UI, click panels, open dropdowns, hover over elements. **No triangle spike artifacts.** The UI should render clean rectangles with no geometric corruption.

## Critical Rules

- Do NOT add fences, semaphores, or GPU synchronization — just fresh buffers per prepare
- Do NOT modify manifold-gpu
- `cargo clippy --workspace -- -D warnings` must pass
- Commit and push when done
