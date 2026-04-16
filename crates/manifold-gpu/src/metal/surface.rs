//! GpuSurface — CAMetalLayer wrapper for presenting rendered content to a window.
//!
//! Wraps a `CAMetalLayer` for acquiring drawables and presenting them.
//! Used by the UI thread for native Metal rendering directly to windows.

use super::device::GpuDevice;
use super::format::to_mtl_pixel_format;
use super::{objc_release, objc_retain};
use crate::metal::types::GpuTexture;
use crate::types::GpuTextureFormat;

use core_graphics_types::geometry::CGSize;

/// A presentable surface backed by a CAMetalLayer.
pub struct GpuSurface {
    /// Retained CAMetalLayer pointer.
    layer_ptr: *mut std::ffi::c_void,
    pub width: u32,
    pub height: u32,
    pub format: GpuTextureFormat,
}

unsafe impl Send for GpuSurface {}

/// A drawable acquired from a GpuSurface.
/// Must be presented before being dropped (or the drawable is wasted).
pub struct GpuDrawable {
    /// Raw CAMetalDrawable pointer (retained).
    drawable_ptr: *mut std::ffi::c_void,
}

unsafe impl Send for GpuDrawable {}

// ─── CGColorSpace FFI ────────────────────────────────────────────────

unsafe extern "C" {
    fn CGColorSpaceCreateWithName(name: *const std::ffi::c_void) -> *mut std::ffi::c_void;
    fn CGColorSpaceRelease(space: *mut std::ffi::c_void);
    fn CGColorCreateGenericRGB(r: f64, g: f64, b: f64, a: f64) -> *mut std::ffi::c_void;
    fn CGColorRelease(color: *mut std::ffi::c_void);
    static kCGColorSpaceExtendedLinearSRGB: *const std::ffi::c_void;
}

// ─── GpuDevice: create_surface ───────────────────────────────────────

impl GpuDevice {
    /// Create a presentable surface backed by a CAMetalLayer.
    ///
    /// Extracts the NSView from the window handle and attaches a configured
    /// CAMetalLayer to it.
    ///
    /// # Safety
    /// The window must remain valid for the lifetime of the returned surface.
    pub fn create_surface(
        &self,
        window: &impl raw_window_handle::HasWindowHandle,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        vsync: bool,
    ) -> GpuSurface {
        use metal::foreign_types::ForeignType;
        use raw_window_handle::RawWindowHandle;

        let ns_view = match window.window_handle().unwrap().as_raw() {
            RawWindowHandle::AppKit(h) => h.ns_view.as_ptr() as *mut objc::runtime::Object,
            _ => panic!("Expected AppKit window handle"),
        };

        let layer = metal::MetalLayer::new();
        let layer_ref: &metal::MetalLayerRef = &layer;
        layer_ref.set_device(self.raw_device());
        layer_ref.set_pixel_format(to_mtl_pixel_format(format));
        layer_ref.set_framebuffer_only(true);
        layer_ref.set_display_sync_enabled(vsync);
        layer_ref.set_maximum_drawable_count(3);
        layer_ref.set_drawable_size(CGSize::new(width as f64, height as f64));
        layer_ref.set_contents_scale(1.0);

        // Attach CAMetalLayer to the NSView.
        let layer_ptr = layer.as_ptr() as *mut std::ffi::c_void;
        unsafe {
            // Prevent CAMetalLayer::nextDrawable from blocking the caller when
            // all drawables are in flight. The output presenter now runs inline
            // on the main frame encoder, so blocking here would stall the app.
            let _: () = msg_send![layer_ptr as *mut objc::runtime::Object,
                setAllowsNextDrawableTimeout: true];
            let _: () = msg_send![ns_view, setLayer: layer_ptr];
            let _: () = msg_send![ns_view, setWantsLayer: true];
            // Retain the layer — the local MetalLayer will drop its reference.
            objc_retain(layer_ptr);
        }

        GpuSurface {
            layer_ptr,
            width,
            height,
            format,
        }
    }
}

// ─── GpuSurface methods ─────────────────────────────────────────────

impl GpuSurface {
    /// Get the raw CAMetalLayer as a MetalLayerRef.
    fn layer_ref(&self) -> &metal::MetalLayerRef {
        unsafe { &*(self.layer_ptr as *const metal::MetalLayerRef) }
    }

    /// Raw CAMetalLayer pointer for interop (e.g. CAMetalDisplayLink).
    pub fn raw_layer_ptr(&self) -> *mut std::ffi::c_void {
        self.layer_ptr
    }

    /// Resize the drawable surface.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.layer_ref()
            .set_drawable_size(CGSize::new(width as f64, height as f64));
    }

    /// Set the maximum number of drawables in the pool (2 or 3).
    /// 2 = lowest latency (blocks until previous present hits display).
    /// 3 = higher throughput, up to 2 frames queue-ahead.
    pub fn set_maximum_drawable_count(&self, count: u32) {
        self.layer_ref()
            .set_maximum_drawable_count(count.clamp(2, 3) as u64);
    }

    /// Acquire the next drawable from the surface.
    /// Returns `None` if no drawable is available (all in-flight).
    pub fn next_drawable(&self) -> Option<GpuDrawable> {
        let drawable = self.layer_ref().next_drawable()?;
        // Retain the drawable so it outlives the autorelease pool.
        let ptr = drawable as *const metal::MetalDrawableRef as *mut std::ffi::c_void;
        unsafe {
            objc_retain(ptr);
        }
        Some(GpuDrawable { drawable_ptr: ptr })
    }

    /// Configure the surface for Extended Dynamic Range (EDR) output.
    /// Sets the colorspace to ExtendedLinearSRGB and enables EDR content.
    pub fn configure_edr(&self) {
        unsafe {
            let cs = CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearSRGB);
            if !cs.is_null() {
                let layer = self.layer_ptr as *mut objc::runtime::Object;
                let _: () = msg_send![layer, setColorspace: cs];
                let _: () = msg_send![layer, setWantsExtendedDynamicRangeContent: true];
                CGColorSpaceRelease(cs);
            }
        }
    }

    /// Set the layer's contents gravity to resize-aspect (letterbox).
    pub fn set_contents_gravity_resize_aspect(&self) {
        unsafe {
            let layer = self.layer_ptr as *mut objc::runtime::Object;
            let gravity: *const objc::runtime::Object = msg_send![class!(NSString), stringWithUTF8String:
                    c"resizeAspect".as_ptr()];
            let _: () = msg_send![layer, setContentsGravity: gravity];
        }
    }

    /// Control whether presents are synchronized with Core Animation transactions.
    /// `false` (recommended for CVDisplayLink): presents are not batched into
    /// CA transactions, preserving the timing guarantees of the display link.
    pub fn set_presents_with_transaction(&self, enabled: bool) {
        unsafe {
            let layer = self.layer_ptr as *mut objc::runtime::Object;
            let _: () = msg_send![layer, setPresentsWithTransaction: enabled];
        }
    }

    /// Set the layer's background color.
    pub fn set_background_color(&self, r: f64, g: f64, b: f64, a: f64) {
        unsafe {
            let color = CGColorCreateGenericRGB(r, g, b, a);
            if !color.is_null() {
                let layer = self.layer_ptr as *mut objc::runtime::Object;
                let _: () = msg_send![layer, setBackgroundColor: color];
                CGColorRelease(color);
            }
        }
    }
}

impl Drop for GpuSurface {
    fn drop(&mut self) {
        if !self.layer_ptr.is_null() {
            unsafe {
                objc_release(self.layer_ptr);
            }
        }
    }
}

// ─── GpuDrawable methods ─────────────────────────────────────────────

impl GpuDrawable {
    /// Create a GpuDrawable from a raw `id<CAMetalDrawable>` pointer.
    /// Retains the drawable. Used by the output presenter which receives
    /// drawables from CAMetalDisplayLink (not from nextDrawable).
    ///
    /// # Safety
    /// `ptr` must be a valid, non-null `id<CAMetalDrawable>`.
    pub unsafe fn from_raw(ptr: *mut std::ffi::c_void) -> Self {
        unsafe { objc_retain(ptr); }
        Self { drawable_ptr: ptr }
    }

    /// Get the drawable's backing texture as a raw pointer.
    /// The returned texture is valid as a render target for the current frame.
    pub fn texture(&self) -> &metal::TextureRef {
        let drawable = unsafe { &*(self.drawable_ptr as *const metal::MetalDrawableRef) };
        drawable.texture()
    }

    /// Get the raw drawable pointer for command buffer present integration.
    pub(crate) fn raw_drawable_ref(&self) -> &metal::MetalDrawableRef {
        unsafe { &*(self.drawable_ptr as *const metal::MetalDrawableRef) }
    }

    /// Create a GpuTexture referencing this drawable's backing texture.
    /// The drawable must outlive the returned GpuTexture.
    /// Retains the Metal texture — safe to use alongside the drawable.
    pub fn gpu_texture(&self, format: GpuTextureFormat) -> GpuTexture {
        use metal::foreign_types::{ForeignType, ForeignTypeRef};
        let tex_ref = self.texture();
        let w = tex_ref.width() as u32;
        let h = tex_ref.height() as u32;
        // Retain: GpuTexture::drop will release, so we need our own +1
        let ptr = tex_ref.as_ptr() as *mut std::ffi::c_void;
        unsafe {
            super::objc_retain(ptr);
        }
        let mtl_texture = unsafe { metal::Texture::from_ptr(ptr as *mut _) };
        GpuTexture::from_raw(mtl_texture, w, h, 1, format)
    }

    /// Present the drawable directly (for `presentsWithTransaction` mode).
    ///
    /// Call AFTER `GpuEncoder::commit_and_wait_scheduled()` to sync with
    /// Core Animation transactions. The drawable is presented immediately
    /// and will be composited on the next WindowServer cycle.
    pub fn present_after_scheduled(&self) {
        let drawable = unsafe { &*(self.drawable_ptr as *const metal::MetalDrawableRef) };
        use std::ops::Deref;
        drawable.deref().present();
    }

    /// Present the drawable immediately.
    /// Consumes self — the drawable is scheduled for display at the next vsync.
    pub fn present(self) {
        let drawable = unsafe { &*(self.drawable_ptr as *const metal::MetalDrawableRef) };
        // MetalDrawableRef derefs to DrawableRef which has present().
        use std::ops::Deref;
        drawable.deref().present();
        // Don't release in drop — present() transfers ownership to the display pipeline.
        // Actually, we still need to release our retain. Drop will handle it.
    }
}

impl Drop for GpuDrawable {
    fn drop(&mut self) {
        if !self.drawable_ptr.is_null() {
            unsafe {
                objc_release(self.drawable_ptr);
            }
        }
    }
}

// ─── GpuEncoder integration ─────────────────────────────────────────

impl super::encoder::GpuEncoder {
    /// Schedule a drawable for presentation when the command buffer completes.
    /// This is preferred over `GpuDrawable::present()` because it coordinates
    /// with the command buffer's GPU work — the drawable is presented only after
    /// all preceding GPU commands finish.
    pub fn present_drawable(&mut self, drawable: &GpuDrawable) {
        // MetalDrawableRef derefs to DrawableRef, which is what
        // CommandBufferRef::present_drawable expects.
        self.cmd_buf().present_drawable(drawable.raw_drawable_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    #[test]
    fn test_indexed_draw_to_texture() {
        // 1. Create GpuDevice
        let device = GpuDevice::new();

        // 2. Create a small render target (4x4, Rgba8Unorm)
        let target = device.create_texture(&GpuTextureDesc {
            width: 4,
            height: 4,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
            label: "test target",
            mip_levels: 1,
        });

        // 3. Define vertex layout: position (Float32x2) + color (Float32x4)
        let layout = GpuVertexLayout {
            stride: 24,
            attributes: vec![
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x4,
                    offset: 8,
                    shader_location: 1,
                },
            ],
        };

        // 4. Create pipeline with vertex layout
        let wgsl = r#"
            struct VertexInput {
                @location(0) position: vec2<f32>,
                @location(1) color: vec4<f32>,
            };
            struct VertexOutput {
                @builtin(position) position: vec4<f32>,
                @location(0) color: vec4<f32>,
            };
            @vertex
            fn vs_main(in: VertexInput) -> VertexOutput {
                var out: VertexOutput;
                out.position = vec4<f32>(in.position, 0.0, 1.0);
                out.color = in.color;
                return out;
            }
            @fragment
            fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
                return in.color;
            }
        "#;

        let pipeline = device.create_render_pipeline_with_vertex_layout(
            wgsl,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Rgba8Unorm,
            None,
            &layout,
            "test pipeline",
        );

        // 5. Create vertex buffer (fullscreen quad as 2 triangles)
        #[repr(C)]
        struct Vertex {
            pos: [f32; 2],
            color: [f32; 4],
        }
        let vertices = [
            Vertex {
                pos: [-1.0, -1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [1.0, -1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [1.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Vertex {
                pos: [-1.0, 1.0],
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ];
        let vertex_data = unsafe {
            std::slice::from_raw_parts(
                vertices.as_ptr() as *const u8,
                std::mem::size_of_val(&vertices),
            )
        };
        let vertex_buffer = device.create_buffer_shared(vertex_data.len() as u64);
        unsafe {
            vertex_buffer.write(0, vertex_data);
        }

        // 6. Create index buffer (two triangles: 0,1,2 + 0,2,3)
        let indices: [u32; 6] = [0, 1, 2, 0, 2, 3];
        let index_data = unsafe {
            std::slice::from_raw_parts(
                indices.as_ptr() as *const u8,
                std::mem::size_of_val(&indices),
            )
        };
        let index_buffer = device.create_buffer_shared(index_data.len() as u64);
        unsafe {
            index_buffer.write(0, index_data);
        }

        // 7. Draw
        let mut encoder = device.create_encoder("test draw");
        encoder.draw_indexed(
            &pipeline,
            &target,
            &[],
            &vertex_buffer,
            0,
            &index_buffer,
            6,
            None,
            GpuLoadAction::Clear,
            "test indexed draw",
        );
        encoder.commit();

        // If we get here without panicking, the pipeline creation
        // and draw_indexed encoding worked correctly.
    }
}
