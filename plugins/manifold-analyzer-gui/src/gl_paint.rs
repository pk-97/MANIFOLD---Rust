//! GL-side of the zero-copy bridge: bind an IOSurface as a GL_TEXTURE_RECTANGLE,
//! render it into egui's GL context as a textured quad via a custom shader.
//!
//! Runs inside `egui::PaintCallback` where the CGL context is current on the
//! calling thread.
//!
//! **Why GL_TEXTURE_RECTANGLE and not GL_TEXTURE_2D:** on macOS Core profile GL
//! (which `egui-baseview` uses), `CGLTexImageIOSurface2D` with `GL_TEXTURE_2D`
//! returns `kCGLBadValue` (10008). Only `GL_TEXTURE_RECTANGLE` is accepted.
//! Rectangle textures sample with pixel coords (0..w, 0..h), not normalized
//! (0..1), so the shaders have to match.

use crate::gpu_bridge::IOSurfaceRef;
use glow::HasContext;
use std::ffi::c_void;
use std::os::raw::c_int;
use std::sync::Arc;

// ─── CGL / IOSurface-to-GL FFI ──────────────────────────────────────

type CGLContextObj = *mut c_void;

#[link(name = "OpenGL", kind = "framework")]
unsafe extern "C" {
    fn CGLGetCurrentContext() -> CGLContextObj;
    fn CGLTexImageIOSurface2D(
        ctx: CGLContextObj,
        target: u32,
        internal_format: u32,
        width: c_int,
        height: c_int,
        format: u32,
        pixel_type: u32,
        ios: IOSurfaceRef,
        plane: u32,
    ) -> c_int;
}

const GL_TEXTURE_RECTANGLE: u32 = 0x84F5;
const GL_RGBA: u32 = 0x1908;
const GL_BGRA: u32 = 0x80E1;
const GL_UNSIGNED_INT_8_8_8_8_REV: u32 = 0x8367;

// ─── Quad painter — cached GL program + VAO ─────────────────────────

pub struct QuadPainter {
    program: glow::Program,
    vao: glow::VertexArray,
    gl_texture: glow::Texture,
    size_uniform: Option<glow::UniformLocation>,
    width: f32,
    height: f32,
}

impl QuadPainter {
    /// One-time GL setup. Must be called with a current GL context (i.e. from
    /// inside a `PaintCallback`). Returns `None` on failure.
    pub fn new(
        gl: &glow::Context,
        iosurface: IOSurfaceRef,
        width: u32,
        height: u32,
    ) -> Option<Self> {
        eprintln!(
            "manifold-analyzer-gui: QuadPainter::new {}x{} iosurface={:p}",
            width, height, iosurface
        );
        unsafe {
            // GL 3.2 Core + sampler2DRect (pixel-coord sampling, matches IOSurface binding).
            let vs_src = "#version 150 core
                out vec2 v_uv_px;
                uniform vec2 u_size;
                void main() {
                    vec2 pos = vec2((gl_VertexID == 2) ? 3.0 : -1.0,
                                    (gl_VertexID == 1) ? 3.0 : -1.0);
                    vec2 norm = (pos + 1.0) * 0.5;
                    norm.y = 1.0 - norm.y;
                    v_uv_px = norm * u_size;
                    gl_Position = vec4(pos, 0.0, 1.0);
                }";
            let fs_src = "#version 150 core
                in vec2 v_uv_px;
                out vec4 frag_color;
                uniform sampler2DRect u_tex;
                void main() { frag_color = texture(u_tex, v_uv_px); }";

            let program = gl.create_program().ok()?;

            let vs = gl.create_shader(glow::VERTEX_SHADER).ok()?;
            gl.shader_source(vs, vs_src);
            gl.compile_shader(vs);
            if !gl.get_shader_compile_status(vs) {
                eprintln!(
                    "manifold-analyzer-gui: VS compile failed: {}",
                    gl.get_shader_info_log(vs)
                );
                gl.delete_shader(vs);
                gl.delete_program(program);
                return None;
            }

            let fs = gl.create_shader(glow::FRAGMENT_SHADER).ok()?;
            gl.shader_source(fs, fs_src);
            gl.compile_shader(fs);
            if !gl.get_shader_compile_status(fs) {
                eprintln!(
                    "manifold-analyzer-gui: FS compile failed: {}",
                    gl.get_shader_info_log(fs)
                );
                gl.delete_shader(vs);
                gl.delete_shader(fs);
                gl.delete_program(program);
                return None;
            }

            gl.attach_shader(program, vs);
            gl.attach_shader(program, fs);
            gl.link_program(program);
            gl.delete_shader(vs);
            gl.delete_shader(fs);
            if !gl.get_program_link_status(program) {
                eprintln!(
                    "manifold-analyzer-gui: link failed: {}",
                    gl.get_program_info_log(program)
                );
                gl.delete_program(program);
                return None;
            }

            let size_uniform = gl.get_uniform_location(program, "u_size");

            let vao = gl.create_vertex_array().ok()?;

            let gl_texture = gl.create_texture().ok()?;
            gl.bind_texture(GL_TEXTURE_RECTANGLE, Some(gl_texture));
            gl.tex_parameter_i32(
                GL_TEXTURE_RECTANGLE,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                GL_TEXTURE_RECTANGLE,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                GL_TEXTURE_RECTANGLE,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                GL_TEXTURE_RECTANGLE,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );

            let ctx = CGLGetCurrentContext();
            if ctx.is_null() {
                eprintln!("manifold-analyzer-gui: CGLGetCurrentContext returned null");
                gl.bind_texture(GL_TEXTURE_RECTANGLE, None);
                return None;
            }
            let rc = CGLTexImageIOSurface2D(
                ctx,
                GL_TEXTURE_RECTANGLE,
                GL_RGBA,
                width as c_int,
                height as c_int,
                GL_BGRA,
                GL_UNSIGNED_INT_8_8_8_8_REV,
                iosurface,
                0,
            );
            gl.bind_texture(GL_TEXTURE_RECTANGLE, None);
            if rc != 0 {
                eprintln!("manifold-analyzer-gui: CGLTexImageIOSurface2D failed: {rc}");
                return None;
            }

            eprintln!("manifold-analyzer-gui: QuadPainter ready");
            Some(Self {
                program,
                vao,
                gl_texture,
                size_uniform,
                width: width as f32,
                height: height as f32,
            })
        }
    }

    /// Draw the IOSurface-backed texture covering the current GL viewport.
    pub fn draw(&self, gl: &glow::Context) {
        unsafe {
            gl.use_program(Some(self.program));
            gl.bind_vertex_array(Some(self.vao));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(GL_TEXTURE_RECTANGLE, Some(self.gl_texture));
            if let Some(loc) = gl.get_uniform_location(self.program, "u_tex") {
                gl.uniform_1_i32(Some(&loc), 0);
            }
            if let Some(ref loc) = self.size_uniform {
                gl.uniform_2_f32(Some(loc), self.width, self.height);
            }
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::CULL_FACE);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);

            gl.bind_vertex_array(None);
            gl.use_program(None);
            gl.bind_texture(GL_TEXTURE_RECTANGLE, None);
        }
    }
}

/// Tri-state: never tried / tried and failed (don't retry every frame) / ready.
#[derive(Default)]
pub enum PainterState {
    #[default]
    NotYet,
    Failed,
    Ready(QuadPainter),
}

pub type SharedPainterState = Arc<std::sync::Mutex<PainterState>>;
