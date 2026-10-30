//! Light from glowing surfaces without ray tracing. Each emissive mesh's
//! glowing area and that area's centre and spread are summed on the GPU
//! (`shaders/glow_gather.wgsl`) and read back once their frame retires; the
//! scene then lights with four soft, unshadowed point lights spread over the
//! area. A point light's Legacy falloff, strength × r² / (r² + d²), is the
//! light a glowing disc of radius r and radiance L casts on a surface facing
//! it from distance d when the strength is π L: up close a surface sees the
//! glow's own brightness, far away the disc's area times L over d². The four
//! discs together cover a quarter of the glowing area, what a round closed
//! surface shows any one direction.

use std::f32::consts::PI;

use manifold_gpu::{BufferUse, FrameClock, GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice};
use manifold_node_engine::content_revision::ContentVersion;
use manifold_node_engine::scene::light::{Light, ShadowSoftness};

use super::ObjectDraw;

const SHADER: &str = include_str!("../shaders/glow_gather.wgsl");
/// `GLOW_GROUPS` in the shader: the partial sums it writes, four vec4s each.
const GLOW_GROUPS: usize = 64;
const PARTIAL_BYTES: u64 = (GLOW_GROUPS * 4 * 16) as u64;
/// Readbacks in flight per mesh.
const READBACK_SLOTS: usize = 3;
/// The most glowing meshes that light the scene, the brightest first: each
/// adds four lights that every lit pixel sums.
const MAX_SOURCES: usize = 4;

/// `GlowParams` in the shader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlowParams {
    model: [[f32; 4]; 4],
    origin: [f32; 4],
    elements: [u32; 4],
}

/// What a mesh's last gather read: unchanged, a static mesh's sums stand.
#[derive(Clone, Copy, PartialEq)]
struct GlowKey {
    vertices: ContentVersion,
    indices: Option<ContentVersion>,
    map: Option<ContentVersion>,
    model: [[u32; 4]; 4],
    elements: u32,
}

/// A glowing mesh as last read back: its area, the area's centre and its
/// spread (covariance) about the centre in the world, and the emissive map's
/// mean colour (white without a map).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Glow {
    area: f32,
    centre: [f32; 3],
    spread: [[f32; 3]; 3],
    map: [f32; 3],
}

impl Glow {
    /// From the area-weighted sums about `origin`: area, area × (x, y, z),
    /// then area × (xx, xy, xz, yy, yz, zz).
    fn from_sums(sums: &[f64; 10], origin: [f32; 3], map: [f32; 3]) -> Option<Self> {
        let area = sums[0];
        if !(area.is_finite() && area > 1e-9) {
            return None;
        }
        let mean = [sums[1] / area, sums[2] / area, sums[3] / area];
        let cov = |sum: f64, a: usize, b: usize| (sum / area - mean[a] * mean[b]) as f32;
        let (xx, xy, xz) = (cov(sums[4], 0, 0), cov(sums[5], 0, 1), cov(sums[6], 0, 2));
        let (yy, yz, zz) = (cov(sums[7], 1, 1), cov(sums[8], 1, 2), cov(sums[9], 2, 2));
        Some(Self {
            area: area as f32,
            centre: [0, 1, 2].map(|i| origin[i] + mean[i] as f32),
            spread: [[xx, xy, xz], [xy, yy, yz], [xz, yz, zz]],
            map,
        })
    }

    /// From one readback: the groups' partial sums, and group 0's map mean
    /// when the map was read.
    fn from_partials(partials: &[[f32; 4]], origin: [f32; 3], mapped: bool) -> Option<Self> {
        let mut sums = [0.0f64; 10];
        for group in partials.chunks_exact(4) {
            let words = [group[0], group[1], [group[2][0], group[2][1], 0.0, 0.0]];
            for (i, value) in words.iter().flatten().take(10).enumerate() {
                sums[i] += f64::from(*value);
            }
        }
        let map = if mapped {
            [partials[3][0], partials[3][1], partials[3][2]]
        } else {
            [1.0; 3]
        };
        Self::from_sums(&sums, origin, map)
    }

    /// The four lights standing in for this glow at `emission` (the
    /// material's emission times its strength): one each side of the centre
    /// along the two widest axes, a standard deviation out.
    fn lights(&self, emission: [f32; 3]) -> [Light; 4] {
        let (values, axes) = symmetric_eigen(self.spread);
        let radiance = [0, 1, 2].map(|i| emission[i] * self.map[i]);
        let radius = (self.area / 16.0 / PI).sqrt();
        let along = |axis: usize, sign: f32| {
            let reach = sign * values[axis].max(0.0).sqrt();
            [0, 1, 2].map(|i| self.centre[i] + reach * axes[axis][i])
        };
        [along(0, 1.0), along(0, -1.0), along(1, 1.0), along(1, -1.0)].map(|pos| {
            Light::point(
                pos,
                [pos[0], pos[1] - 1.0, pos[2]],
                radiance,
                PI,
                radius,
                false,
                ShadowSoftness::Hard,
                0.0,
                0,
            )
        })
    }
}

/// Eigenvalues and unit eigenvectors of a symmetric 3×3 matrix, largest
/// first, by cyclic Jacobi rotations.
fn symmetric_eigen(m: [[f32; 3]; 3]) -> ([f32; 3], [[f32; 3]; 3]) {
    let mut a = m.map(|row| row.map(f64::from));
    let mut v = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0f64]];
    for _ in 0..12 {
        for (p, q) in [(0, 1), (0, 2), (1, 2)] {
            if a[p][q].abs() < 1e-15 {
                continue;
            }
            let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
            let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
            let c = 1.0 / (t * t + 1.0).sqrt();
            let s = t * c;
            for row in &mut a {
                let (kp, kq) = (row[p], row[q]);
                row[p] = c * kp - s * kq;
                row[q] = s * kp + c * kq;
            }
            let (row_p, row_q) = (a[p], a[q]);
            a[p] = [0, 1, 2].map(|k| c * row_p[k] - s * row_q[k]);
            a[q] = [0, 1, 2].map(|k| s * row_p[k] + c * row_q[k]);
            for row in &mut v {
                let (kp, kq) = (row[p], row[q]);
                row[p] = c * kp - s * kq;
                row[q] = s * kp + c * kq;
            }
        }
    }
    let mut order = [0, 1, 2];
    order.sort_by(|&i, &j| a[j][j].total_cmp(&a[i][i]));
    (
        order.map(|i| a[i][i] as f32),
        order.map(|i| [v[0][i] as f32, v[1][i] as f32, v[2][i] as f32]),
    )
}

struct ReadbackSlot {
    buffer: GpuBuffer,
    stamp: u64,
    pending: bool,
    origin: [f32; 3],
    mapped: bool,
}

/// One glowing mesh, by its render slot.
struct Source {
    slot: u32,
    partials: GpuBuffer,
    readback: Vec<ReadbackSlot>,
    glow: Option<Glow>,
    key: Option<GlowKey>,
    /// The material's emission times its strength, from the latest frame.
    emission: [f32; 3],
    seen: bool,
}

impl Source {
    fn power(&self) -> f32 {
        self.glow.map_or(0.0, |glow| {
            let lum = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
            glow.area * lum([0, 1, 2].map(|i| self.emission[i] * glow.map[i]))
        })
    }

    /// Take every retired readback, keeping the newest.
    fn poll(&mut self, clock: Option<&FrameClock>) {
        let mut newest: Option<(u64, Option<Glow>)> = None;
        for slot in self.readback.iter_mut().filter(|s| s.pending) {
            if !clock.is_none_or(|c| c.is_complete(slot.stamp)) {
                continue;
            }
            slot.pending = false;
            let ptr = slot
                .buffer
                .mapped_ptr()
                .expect("glow readback must be CPU-visible");
            // SAFETY: the frame that wrote this shared slot has retired.
            let partials = unsafe {
                std::slice::from_raw_parts(ptr.cast::<[f32; 4]>().cast_const(), GLOW_GROUPS * 4)
            };
            let glow = Glow::from_partials(partials, slot.origin, slot.mapped);
            if newest.is_none_or(|(stamp, _)| slot.stamp >= stamp) {
                newest = Some((slot.stamp, glow));
            }
        }
        if let Some((_, glow)) = newest {
            self.glow = glow;
        }
    }
}

#[derive(Default)]
pub(super) struct GlowPass {
    gather: Option<GpuComputePipeline>,
    map_mean: Option<GpuComputePipeline>,
    /// Brightest first, as of the last poll.
    sources: Vec<Source>,
}

impl GlowPass {
    /// The lights standing in for the glow last read back.
    pub(super) fn lights(&self) -> impl Iterator<Item = Light> + '_ {
        self.sources
            .iter()
            .filter_map(|source| source.glow.map(|glow| glow.lights(source.emission)))
            .take(MAX_SOURCES)
            .flatten()
    }

    /// No glow lights: ray-traced light gathers the glow itself.
    pub(super) fn clear(&mut self) {
        self.sources.clear();
    }

    /// Read back what has retired, then gather every emissive filled mesh
    /// drawn once (copies and points give no glow). A mesh no longer drawn
    /// or no longer glowing stops lighting.
    pub(super) fn gather(
        &mut self,
        device: &GpuDevice,
        encoder: &mut manifold_gpu::GpuEncoder,
        draws: &[ObjectDraw<'_>],
    ) {
        let clock = device.frame_clock();
        for source in &mut self.sources {
            source.poll(clock.as_ref());
            source.seen = false;
        }
        for draw in draws {
            let e = draw.uniforms.emission;
            let emission = [e[0] * e[3], e[1] * e[3], e[2] * e[3]];
            if draw.points || draw.instances.is_some() || emission.iter().all(|&c| c <= 0.0) {
                continue;
            }
            let map = draw
                .emissive_map
                .filter(|_| draw.uniforms.texture_flags2[2] > 0.5);
            let model = draw.uniforms.model;
            let key = draw
                .vertices_content
                .filter(|_| draw.live_extent.is_none() && draw.geometry_content_known)
                .map(|vertices| GlowKey {
                    vertices,
                    indices: draw.indices_content,
                    map: draw.rt_texture_content[3],
                    model: model.map(|c| c.map(f32::to_bits)),
                    elements: draw.vertex_count,
                });
            let index = match self.sources.iter().position(|s| s.slot == draw.slot) {
                Some(index) => index,
                None => {
                    self.sources.push(Source {
                        slot: draw.slot,
                        partials: device.create_buffer(PARTIAL_BYTES, BufferUse::Gpu),
                        readback: Vec::new(),
                        glow: None,
                        key: None,
                        emission,
                        seen: false,
                    });
                    self.sources.len() - 1
                }
            };
            let source = &mut self.sources[index];
            source.seen = true;
            source.emission = emission;
            if key.is_some() && source.key == key && source.glow.is_some() {
                continue;
            }
            let complete = |slot: &ReadbackSlot| {
                !slot.pending || clock.as_ref().is_none_or(|c| c.is_complete(slot.stamp))
            };
            let slot = match source.readback.iter().position(complete) {
                Some(slot) => slot,
                None if source.readback.len() < READBACK_SLOTS => {
                    source.readback.push(ReadbackSlot {
                        buffer: device.create_buffer(PARTIAL_BYTES, BufferUse::Readback),
                        stamp: 0,
                        pending: false,
                        origin: [0.0; 3],
                        mapped: false,
                    });
                    source.readback.len() - 1
                }
                // Every slot is still in flight: gather next frame; live
                // never waits on the GPU.
                None => continue,
            };
            let gather = self.gather.get_or_insert_with(|| {
                device.create_compute_pipeline(
                    SHADER,
                    "glow_gather",
                    "node.render_scene glow gather",
                )
            });
            let origin = [model[3][0], model[3][1], model[3][2]];
            let params = GlowParams {
                model,
                origin: [origin[0], origin[1], origin[2], 0.0],
                elements: [
                    draw.vertex_count,
                    u32::from(draw.indices.is_some()),
                    draw.live_extent
                        .as_ref()
                        .map_or(u32::MAX, |e| (e.offset / 4) as u32),
                    draw.live_extent.as_ref().map_or(1, |e| e.per_item.max(1)),
                ],
            };
            encoder.dispatch_compute(
                gather,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&params),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: draw.vertices,
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 2,
                        buffer: draw.indices.unwrap_or(draw.vertices),
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 3,
                        buffer: draw
                            .live_extent
                            .as_ref()
                            .map_or(draw.vertices, |e| &e.counts),
                        offset: 0,
                    },
                    GpuBinding::Buffer {
                        binding: 4,
                        buffer: &source.partials,
                        offset: 0,
                    },
                ],
                [GLOW_GROUPS as u32, 1, 1],
                "node.render_scene glow gather",
            );
            if let Some(map) = map {
                let mean = self.map_mean.get_or_insert_with(|| {
                    device.create_compute_pipeline(
                        SHADER,
                        "glow_map_mean",
                        "node.render_scene glow map mean",
                    )
                });
                encoder.dispatch_compute(
                    mean,
                    &[
                        GpuBinding::Buffer {
                            binding: 4,
                            buffer: &source.partials,
                            offset: 0,
                        },
                        GpuBinding::Texture {
                            binding: 5,
                            texture: map,
                        },
                    ],
                    [1, 1, 1],
                    "node.render_scene glow map mean",
                );
            }
            let readback = &mut source.readback[slot];
            encoder.copy_buffer_to_buffer(&source.partials, &readback.buffer, PARTIAL_BYTES);
            readback.stamp = clock.as_ref().map_or(0, |c| c.stamp());
            readback.pending = true;
            readback.origin = origin;
            readback.mapped = map.is_some();
            source.key = key;
        }
        self.sources.retain(|source| source.seen);
        self.sources.sort_by(|a, b| b.power().total_cmp(&a.power()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A glowing 3 × 1 m sheet lying flat at (2, 0.5, -1) lights with four
    /// lights a standard deviation out along its length and width, together
    /// as bright far away as the quarter of its area a closed surface shows.
    #[test]
    fn a_flat_sheet_lights_along_its_length_and_width() {
        let (w, d, area) = (3.0f64, 1.0f64, 3.0f64);
        let c = [2.0f64, 0.5, -1.0];
        let second = [
            c[0] * c[0] + w * w / 12.0,
            c[0] * c[1],
            c[0] * c[2],
            c[1] * c[1],
            c[1] * c[2],
            c[2] * c[2] + d * d / 12.0,
        ];
        let mut sums = [0.0; 10];
        sums[0] = area;
        for i in 0..3 {
            sums[1 + i] = area * c[i];
        }
        for i in 0..6 {
            sums[4 + i] = area * second[i];
        }
        let glow = Glow::from_sums(&sums, [0.0; 3], [0.5, 0.25, 0.1]).expect("a sheet glows");
        let lights = glow.lights([2.0, 2.0, 2.0]);
        let reach = [(w * w / 12.0).sqrt() as f32, (d * d / 12.0).sqrt() as f32];
        for (axis, sign) in [(0, 1.0), (0, -1.0), (2, 1.0), (2, -1.0)] {
            let mut expected = [2.0f32, 0.5, -1.0];
            expected[axis] += sign * reach[axis / 2];
            let near = |l: &Light| (0..3).all(|i| (l.pos[i] - expected[i]).abs() < 1e-4);
            assert!(lights.iter().any(near), "no light at {expected:?}");
        }
        for light in &lights {
            assert!((light.color[0] - PI * 1.0).abs() < 1e-4);
            assert!((light.color[1] - PI * 0.5).abs() < 1e-4);
        }
        // Far away each light gives strength × r²; the four together L × A / 4.
        let far: f32 = lights.iter().map(|l| l.color[0] * l.range * l.range).sum();
        assert!((far - 1.0 * area as f32 / 4.0).abs() < 1e-4, "{far}");
    }
}
