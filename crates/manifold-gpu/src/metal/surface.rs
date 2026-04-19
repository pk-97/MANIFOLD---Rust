//! GpuSurface — CAMetalLayer wrapper for presenting rendered content to a window.

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{Encode, Encoding, RefEncode, class, msg_send};
use objc2_foundation::NSString;
use objc2_metal::{MTLCommandBuffer, MTLDrawable, MTLPixelFormat, MTLTexture};

// Opaque CoreFoundation struct types.
#[repr(C)]
struct CGColorSpaceOpaque {
    _priv: [u8; 0],
}
unsafe impl RefEncode for CGColorSpaceOpaque {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColorSpace", &[]));
}

#[repr(C)]
struct CGColorOpaque {
    _priv: [u8; 0],
}
unsafe impl RefEncode for CGColorOpaque {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColor", &[]));
}

use super::device::GpuDevice;
use super::format::to_mtl_pixel_format;
use crate::metal::types::GpuTexture;
use crate::types::GpuTextureFormat;

/// A presentable surface backed by a CAMetalLayer.
pub struct GpuSurface {
    /// Retained CAMetalLayer pointer.
    layer_ptr: *mut c_void,
    pub width: u32,
    pub height: u32,
    pub format: GpuTextureFormat,
}

unsafe impl Send for GpuSurface {}

/// A drawable acquired from a GpuSurface.
pub struct GpuDrawable {
    /// Retained CAMetalDrawable (an `id<CAMetalDrawable>` pointer, retained).
    drawable_ptr: *mut c_void,
}

unsafe impl Send for GpuDrawable {}

// ─── CGColorSpace FFI ────────────────────────────────────────────────

unsafe extern "C" {
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGColorCreateGenericRGB(r: f64, g: f64, b: f64, a: f64) -> *mut c_void;
    fn CGColorRelease(color: *mut c_void);
    static kCGColorSpaceExtendedLinearSRGB: *const c_void;
}

// ─── Low-level CAMetalLayer helpers (objc2 msg_send) ─────────────────

/// Allocate a fresh CAMetalLayer and retain it. Returns +1.
unsafe fn new_metal_layer() -> *mut c_void {
    unsafe {
        let cls = class!(CAMetalLayer);
        let obj: *mut AnyObject = msg_send![cls, alloc];
        let obj: *mut AnyObject = msg_send![obj, init];
        obj as *mut c_void
    }
}

unsafe fn layer_set_device(layer: *mut c_void, device: &ProtocolObject<dyn objc2_metal::MTLDevice>) {
    unsafe {
        let layer_obj: *mut AnyObject = layer.cast();
        let dev_ptr: *const AnyObject = device as *const _ as *const AnyObject;
        let _: () = msg_send![layer_obj, setDevice: dev_ptr];
    }
}

unsafe fn layer_set_pixel_format(layer: *mut c_void, format: MTLPixelFormat) {
    unsafe {
        let layer_obj: *mut AnyObject = layer.cast();
        let raw = format.0;
        let _: () = msg_send![layer_obj, setPixelFormat: raw];
    }
}

unsafe fn layer_set_framebuffer_only(layer: *mut c_void, v: bool) {
    unsafe {
        let layer_obj: *mut AnyObject = layer.cast();
        let _: () = msg_send![layer_obj, setFramebufferOnly: v];
    }
}

unsafe fn layer_set_display_sync_enabled(layer: *mut c_void, v: bool) {
    unsafe {
        let layer_obj: *mut AnyObject = layer.cast();
        let _: () = msg_send![layer_obj, setDisplaySyncEnabled: v];
    }
}

unsafe fn layer_set_maximum_drawable_count(layer: *mut c_void, n: usize) {
    unsafe {
        let layer_obj: *mut AnyObject = layer.cast();
        let _: () = msg_send![layer_obj, setMaximumDrawableCount: n];
    }
}

/// CGSize struct — matches the ABI.
#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct(
        "CGSize",
        &[<f64 as Encode>::ENCODING, <f64 as Encode>::ENCODING],
    );
}

unsafe fn layer_set_drawable_size(layer: *mut c_void, w: f64, h: f64) {
    unsafe {
        let layer_obj: *mut AnyObject = layer.cast();
        let size = CGSize { width: w, height: h };
        let _: () = msg_send![layer_obj, setDrawableSize: size];
    }
}

unsafe fn layer_set_contents_scale(layer: *mut c_void, s: f64) {
    unsafe {
        let layer_obj: *mut AnyObject = layer.cast();
        let _: () = msg_send![layer_obj, setContentsScale: s];
    }
}

/// Acquire the next drawable; returns a +1 retained pointer, or null if unavailable.
unsafe fn layer_next_drawable(layer: *mut c_void) -> *mut c_void {
    unsafe {
        let layer_obj: *mut AnyObject = layer.cast();
        let raw: *mut AnyObject = msg_send![layer_obj, nextDrawable];
        if raw.is_null() {
            return std::ptr::null_mut();
        }
        // nextDrawable returns autoreleased; retain to match our +1 ownership.
        let _: *mut AnyObject = msg_send![raw, retain];
        raw as *mut c_void
    }
}

// ─── GpuDevice: create_surface ───────────────────────────────────────

impl GpuDevice {
    /// Create a presentable surface backed by a CAMetalLayer.
    pub fn create_surface(
        &self,
        window: &impl raw_window_handle::HasWindowHandle,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        vsync: bool,
    ) -> GpuSurface {
        use raw_window_handle::RawWindowHandle;

        let ns_view = match window.window_handle().unwrap().as_raw() {
            RawWindowHandle::AppKit(h) => h.ns_view.as_ptr() as *mut AnyObject,
            _ => panic!("Expected AppKit window handle"),
        };

        let layer_ptr = unsafe { new_metal_layer() };
        unsafe {
            layer_set_device(layer_ptr, self.raw_device());
            layer_set_pixel_format(layer_ptr, to_mtl_pixel_format(format));
            layer_set_framebuffer_only(layer_ptr, true);
            layer_set_display_sync_enabled(layer_ptr, vsync);
            layer_set_maximum_drawable_count(layer_ptr, 3);
            layer_set_drawable_size(layer_ptr, width as f64, height as f64);
            layer_set_contents_scale(layer_ptr, 1.0);

            // Attach CAMetalLayer to the NSView.
            let layer_obj: *mut AnyObject = layer_ptr.cast();
            let _: () = msg_send![layer_obj, setAllowsNextDrawableTimeout: true];
            let _: () = msg_send![ns_view, setLayer: layer_obj];
            let _: () = msg_send![ns_view, setWantsLayer: true];
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
    /// Resize the drawable surface.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        unsafe { layer_set_drawable_size(self.layer_ptr, width as f64, height as f64) };
    }

    /// Set the maximum number of drawables in the pool (2 or 3).
    pub fn set_maximum_drawable_count(&self, count: u32) {
        unsafe {
            layer_set_maximum_drawable_count(self.layer_ptr, count.clamp(2, 3) as usize);
        }
    }

    /// Acquire the next drawable from the surface.
    pub fn next_drawable(&self) -> Option<GpuDrawable> {
        let ptr = unsafe { layer_next_drawable(self.layer_ptr) };
        if ptr.is_null() {
            None
        } else {
            Some(GpuDrawable { drawable_ptr: ptr })
        }
    }

    /// Configure the surface for Extended Dynamic Range (EDR) output.
    pub fn configure_edr(&self) {
        unsafe {
            let cs = CGColorSpaceCreateWithName(kCGColorSpaceExtendedLinearSRGB);
            if !cs.is_null() {
                let layer = self.layer_ptr as *mut AnyObject;
                let cs_ptr: *mut CGColorSpaceOpaque = cs.cast();
                let _: () = msg_send![layer, setColorspace: cs_ptr];
                let _: () = msg_send![layer, setWantsExtendedDynamicRangeContent: true];
                CGColorSpaceRelease(cs);
            }
        }
    }

    /// Set the layer's contents gravity to resize-aspect (letterbox).
    pub fn set_contents_gravity_resize_aspect(&self) {
        unsafe {
            let layer = self.layer_ptr as *mut AnyObject;
            let gravity = NSString::from_str("resizeAspect");
            let gravity_ptr: *const AnyObject = &*gravity as *const NSString as *const AnyObject;
            let _: () = msg_send![layer, setContentsGravity: gravity_ptr];
        }
    }

    /// Control whether presents are synchronized with Core Animation transactions.
    pub fn set_presents_with_transaction(&self, enabled: bool) {
        unsafe {
            let layer = self.layer_ptr as *mut AnyObject;
            let _: () = msg_send![layer, setPresentsWithTransaction: enabled];
        }
    }

    /// Set the layer's background color.
    pub fn set_background_color(&self, r: f64, g: f64, b: f64, a: f64) {
        unsafe {
            let color = CGColorCreateGenericRGB(r, g, b, a);
            if !color.is_null() {
                let layer = self.layer_ptr as *mut AnyObject;
                let color_ptr: *mut CGColorOpaque = color.cast();
                let _: () = msg_send![layer, setBackgroundColor: color_ptr];
                CGColorRelease(color);
            }
        }
    }
}

impl Drop for GpuSurface {
    fn drop(&mut self) {
        if !self.layer_ptr.is_null() {
            // Release the +1 retain from alloc/init.
            unsafe {
                let layer_obj: *mut AnyObject = self.layer_ptr.cast();
                let _: () = msg_send![layer_obj, release];
            }
        }
    }
}

// ─── GpuDrawable methods ─────────────────────────────────────────────

impl GpuDrawable {
    /// Get the drawable's backing texture reference (retained).
    pub fn texture(&self) -> Retained<ProtocolObject<dyn MTLTexture>> {
        unsafe {
            let drawable_obj: *mut AnyObject = self.drawable_ptr.cast();
            let tex_raw: *mut AnyObject = msg_send![drawable_obj, texture];
            assert!(!tex_raw.is_null(), "drawable.texture returned nil");
            // drawable.texture returns autoreleased; retain for our ownership.
            let _: *mut AnyObject = msg_send![tex_raw, retain];
            let typed: *mut ProtocolObject<dyn MTLTexture> = tex_raw.cast();
            Retained::from_raw(typed).expect("drawable.texture returned nil")
        }
    }

    /// Create a GpuTexture referencing this drawable's backing texture.
    pub fn gpu_texture(&self, format: GpuTextureFormat) -> GpuTexture {
        let tex = self.texture();
        let w = unsafe { tex.width() } as u32;
        let h = unsafe { tex.height() } as u32;
        GpuTexture::from_raw(tex, w, h, 1, format)
    }

    /// Present the drawable directly (for `presentsWithTransaction` mode).
    pub fn present_after_scheduled(&self) {
        unsafe {
            let drawable_obj: *mut AnyObject = self.drawable_ptr.cast();
            let _: () = msg_send![drawable_obj, present];
        }
    }

    /// Present the drawable immediately.
    pub fn present(self) {
        unsafe {
            let drawable_obj: *mut AnyObject = self.drawable_ptr.cast();
            let _: () = msg_send![drawable_obj, present];
        }
    }

    /// Internal accessor for command-buffer present integration.
    /// Returns a `ProtocolObject<dyn MTLDrawable>` reference — CAMetalDrawable
    /// conforms to MTLDrawable, so the cast is safe.
    pub(crate) fn raw_drawable(&self) -> &ProtocolObject<dyn MTLDrawable> {
        unsafe { &*(self.drawable_ptr as *const ProtocolObject<dyn MTLDrawable>) }
    }
}

impl Drop for GpuDrawable {
    fn drop(&mut self) {
        if !self.drawable_ptr.is_null() {
            unsafe {
                let drawable_obj: *mut AnyObject = self.drawable_ptr.cast();
                let _: () = msg_send![drawable_obj, release];
            }
        }
    }
}

// ─── GpuEncoder integration ─────────────────────────────────────────

impl super::encoder::GpuEncoder {
    /// Schedule a drawable for presentation when the command buffer completes.
    pub fn present_drawable(&mut self, drawable: &GpuDrawable) {
        unsafe {
            self.cmd_buf().presentDrawable(drawable.raw_drawable());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    #[test]
    fn test_indexed_draw_to_texture() {
        let device = GpuDevice::new();

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
    }
}
