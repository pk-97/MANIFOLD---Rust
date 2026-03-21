//! GPU texture management and rendering for per-layer bitmap textures.
//!
//! Each layer gets a wgpu texture uploaded from CPU pixel buffers produced by
//! `manifold_ui::bitmap_renderer::LayerBitmapRenderer`. Textures are rendered
//! as positioned quads in the viewport area.

use manifold_ui::node::{Color32, Rect};
use wgpu::util::DeviceExt;

/// Vertex for textured quad rendering.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BitmapVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

const BITMAP_SHADER: &str = r#"
struct Globals {
    screen_size: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var t_layer: texture_2d<f32>;
@group(1) @binding(1) var s_layer: sampler;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    // Convert pixel coordinates to NDC: (0,0) top-left, (w,h) bottom-right
    let ndc_x = (in.position.x / globals.screen_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.position.y / globals.screen_size.y) * 2.0;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(t_layer, s_layer, in.uv);
    // Skip fully transparent pixels (bitmap background)
    if color.a < 0.004 {
        discard;
    }
    return color;
}
"#;

/// Per-layer GPU texture.
struct LayerTexture {
    texture: wgpu::Texture,
    _view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    width: u32,
    height: u32,
}

/// Manages GPU textures for all layer bitmaps and renders them as positioned quads.
pub struct LayerBitmapGpu {
    textures: Vec<Option<LayerTexture>>,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    texture_bind_group_layout: wgpu::BindGroupLayout,
    globals_buffer: wgpu::Buffer,
    globals_bind_group_layout: wgpu::BindGroupLayout,
    // Reusable vertex/index buffers
    vertices: Vec<BitmapVertex>,
    indices: Vec<u32>,
}

impl LayerBitmapGpu {
    pub fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Bitmap Layer Shader"),
            source: wgpu::ShaderSource::Wgsl(BITMAP_SHADER.into()),
        });

        // Globals bind group layout (group 0)
        let globals_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Bitmap Globals BGL"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });

        // Texture bind group layout (group 1)
        let texture_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("Bitmap Texture BGL"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Bitmap Pipeline Layout"),
            bind_group_layouts: &[&globals_bind_group_layout, &texture_bind_group_layout],
            immediate_size: 0,
        });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<BitmapVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                wgpu::VertexAttribute {
                    format: wgpu::VertexFormat::Float32x2,
                    offset: 8,
                    shader_location: 1,
                },
            ],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Bitmap Pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[vertex_layout],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // Nearest-neighbor sampler (matches Unity FilterMode.Point)
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Bitmap Sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Bitmap Globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            textures: Vec::new(),
            pipeline,
            sampler,
            texture_bind_group_layout,
            globals_buffer,
            globals_bind_group_layout,
            vertices: Vec::with_capacity(64),
            indices: Vec::with_capacity(96),
        }
    }

    /// Upload CPU pixel buffer to GPU texture for a layer.
    /// Creates or resizes texture as needed.
    pub fn upload_layer(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layer_index: usize,
        pixels: &[Color32],
        tex_w: u32,
        tex_h: u32,
    ) {
        if tex_w == 0 || tex_h == 0 {
            return;
        }

        // Ensure vec is large enough
        if layer_index >= self.textures.len() {
            self.textures.resize_with(layer_index + 1, || None);
        }

        // Check if we need to recreate the texture (size changed)
        let needs_create = match &self.textures[layer_index] {
            Some(lt) => lt.width != tex_w || lt.height != tex_h,
            None => true,
        };

        if needs_create {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("Layer Bitmap {}", layer_index)),
                size: wgpu::Extent3d {
                    width: tex_w,
                    height: tex_h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(&format!("Layer Bitmap BG {}", layer_index)),
                layout: &self.texture_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            self.textures[layer_index] = Some(LayerTexture {
                texture,
                _view: view,
                bind_group,
                width: tex_w,
                height: tex_h,
            });
        }

        // Upload pixel data via queue.write_texture()
        // Color32 is #[repr(C)] with 4 u8 fields — safe to reinterpret as &[u8]
        if let Some(lt) = &self.textures[layer_index] {
            let bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    pixels.as_ptr() as *const u8,
                    pixels.len() * 4,
                )
            };
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &lt.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(tex_w * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: tex_w,
                    height: tex_h,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    /// Render all active layer bitmap textures as positioned quads.
    /// `layer_rects`: vec of `(layer_index, rect)` in logical pixels.
    pub fn render_layers(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        screen_w: u32,
        screen_h: u32,
        layer_rects: &[(usize, Rect)],
    ) {
        if layer_rects.is_empty() {
            return;
        }

        // Update globals
        let globals_data: [f32; 4] = [screen_w as f32, screen_h as f32, 0.0, 0.0];
        queue.write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(&globals_data));

        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Bitmap Globals BG"),
            layout: &self.globals_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: self.globals_buffer.as_entire_binding(),
            }],
        });

        // Build vertex/index data for all layer quads
        self.vertices.clear();
        self.indices.clear();

        // Collect which layers have textures
        let mut draw_list: Vec<(usize, u32, u32)> = Vec::new(); // (layer_idx, vert_start, index_count)

        for &(layer_idx, rect) in layer_rects {
            if layer_idx >= self.textures.len() {
                continue;
            }
            if self.textures[layer_idx].is_none() {
                continue;
            }
            if rect.width <= 0.0 || rect.height <= 0.0 {
                continue;
            }

            let base = self.vertices.len() as u32;
            let (x0, y0) = (rect.x, rect.y);
            let (x1, y1) = (rect.x + rect.width, rect.y + rect.height);

            self.vertices.push(BitmapVertex { position: [x0, y0], uv: [0.0, 0.0] });
            self.vertices.push(BitmapVertex { position: [x1, y0], uv: [1.0, 0.0] });
            self.vertices.push(BitmapVertex { position: [x1, y1], uv: [1.0, 1.0] });
            self.vertices.push(BitmapVertex { position: [x0, y1], uv: [0.0, 1.0] });

            self.indices.extend_from_slice(&[
                base, base + 1, base + 2,
                base, base + 2, base + 3,
            ]);

            draw_list.push((layer_idx, base, 6));
        }

        if self.vertices.is_empty() {
            return;
        }

        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Bitmap Vertices"),
            contents: bytemuck::cast_slice(&self.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("Bitmap Indices"),
            contents: bytemuck::cast_slice(&self.indices),
            usage: wgpu::BufferUsages::INDEX,
        });

        // Render pass — one pass, switch texture bind group per layer
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Bitmap Layer Pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load, // Preserve existing content
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &globals_bind_group, &[]);
        pass.set_vertex_buffer(0, vertex_buffer.slice(..));
        pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint32);

        for (layer_idx, vert_start, index_count) in &draw_list {
            if let Some(lt) = &self.textures[*layer_idx] {
                pass.set_bind_group(1, &lt.bind_group, &[]);
                // Each layer's 6 indices start at (vert_start / 4) * 6
                let index_offset = (vert_start / 4) * 6;
                pass.draw_indexed(index_offset..index_offset + index_count, 0, 0..1);
            }
        }
    }

    /// Remove textures for layers that no longer exist.
    pub fn trim_to_layer_count(&mut self, count: usize) {
        if self.textures.len() > count {
            self.textures.truncate(count);
        }
    }
}

