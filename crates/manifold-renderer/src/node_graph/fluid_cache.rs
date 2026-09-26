//! Bounded-memory, compressed baked fluid snapshots.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::fluid::{FluidSettings, TICK};
use super::transform::Transform;
use crate::generators::mesh_common::{InstanceTransform, MeshVertex};
use crate::node_graph::fluid::WhitewaterFrame;
use manifold_fluids::FrameStats;

const MAGIC: &[u8; 8] = b"MFLUIDC1";
const LEGACY_FORMAT_VERSION: u32 = 3;
const FORMAT_VERSION: u32 = 4;
const MANIFEST: &str = "manifest.bin";
const MAX_VERTICES: usize = 3_145_728;
const MAX_WHITEWATER: usize = 250_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CacheMode {
    Live,
    Record,
    Playback,
}

impl CacheMode {
    pub(crate) fn from_enum(value: u32) -> Option<Self> {
        Some(match value {
            0 => Self::Live,
            1 => Self::Record,
            2 => Self::Playback,
            _ => return None,
        })
    }
}

pub(crate) struct CacheWriter {
    directory: Arc<PathBuf>,
    max_vertices: usize,
    max_whitewater: usize,
}

impl CacheWriter {
    pub(crate) fn create(directory: Arc<PathBuf>, settings: FluidSettings) -> Result<Self, String> {
        if directory.as_os_str().is_empty() {
            return Err("Water cache record path is empty".into());
        }
        if directory.exists()
            && (directory.join(MANIFEST).exists()
                || fs::read_dir(directory.as_ref())
                    .map_err(|error| {
                        format!("Water cache could not inspect its directory: {error}")
                    })?
                    .next()
                    .is_some())
        {
            return Err("Water cache record directory already exists and is not empty".into());
        }
        fs::create_dir_all(directory.as_ref())
            .map_err(|error| format!("Water cache could not create its directory: {error}"))?;
        write_manifest_atomic(directory.as_ref(), settings)?;
        Ok(Self {
            directory,
            max_vertices: settings.max_vertices,
            max_whitewater: settings.whitewater.max_particles as usize,
        })
    }

    pub(crate) fn append(
        &self,
        tick: u64,
        vertices: &[MeshVertex],
        whitewater: &WhitewaterFrame,
        obstacle: Transform,
        stats: FrameStats,
    ) -> Result<(), String> {
        if vertices.len() > self.max_vertices || vertices.len() > MAX_VERTICES {
            return Err("Water cache frame exceeds mesh capacity".into());
        }
        if [&whitewater.foam, &whitewater.bubbles, &whitewater.spray]
            .iter()
            .any(|values| values.len() > self.max_whitewater)
        {
            return Err("Water cache frame exceeds whitewater capacity".into());
        }
        let path = frame_path(self.directory.as_ref(), tick);
        let temporary = temporary_path(&path);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("Water cache could not create temporary frame: {error}"))?;
        let mut encoder = zstd::stream::write::Encoder::new(BufWriter::new(file), 3)
            .map_err(|error| format!("Water cache compression could not start: {error}"))?;
        encoder
            .include_checksum(true)
            .map_err(|error| format!("Water cache checksum setup failed: {error}"))?;
        write_frame(&mut encoder, tick, vertices, whitewater, obstacle, stats)
            .map_err(|error| format!("Water cache frame {tick} could not be written: {error}"))?;
        let mut writer = encoder
            .finish()
            .map_err(|error| format!("Water cache frame {tick} could not finish: {error}"))?;
        writer
            .flush()
            .and_then(|_| writer.get_ref().sync_all())
            .map_err(|error| format!("Water cache frame {tick} could not sync: {error}"))?;
        fs::rename(&temporary, &path).map_err(|error| {
            format!("Water cache frame {tick} could not publish atomically: {error}")
        })
    }
}

pub(crate) struct CacheReader {
    directory: Arc<PathBuf>,
    max_vertices: usize,
    max_whitewater: usize,
}

impl CacheReader {
    pub(crate) fn open(directory: Arc<PathBuf>, settings: FluidSettings) -> Result<Self, String> {
        if directory.as_os_str().is_empty() {
            return Err("Water cache playback path is empty".into());
        }
        let manifest = File::open(directory.join(MANIFEST))
            .map_err(|error| format!("Water cache playback could not open manifest: {error}"))?;
        let mut reader = BufReader::new(manifest);
        read_manifest(&mut reader, settings)
            .map_err(|error| format!("Water cache playback rejected manifest: {error}"))?;
        Ok(Self {
            directory,
            max_vertices: settings.max_vertices,
            max_whitewater: settings.whitewater.max_particles as usize,
        })
    }

    pub(crate) fn read_into(
        &self,
        tick: u64,
        vertices: &mut Vec<MeshVertex>,
        whitewater: &mut WhitewaterFrame,
    ) -> Result<(Transform, FrameStats), String> {
        let path = frame_path(self.directory.as_ref(), tick);
        let file = File::open(&path)
            .map_err(|error| format!("Water cache has no baked frame for tick {tick}: {error}"))?;
        let mut decoder = zstd::stream::read::Decoder::new(BufReader::new(file))
            .map_err(|error| format!("Water cache frame {tick} could not decompress: {error}"))?;
        decoder
            .window_log_max(23)
            .map_err(|error| format!("Water cache decompression limit failed: {error}"))?;
        let (decoded_tick, obstacle, stats) = read_frame(
            &mut decoder,
            vertices,
            whitewater,
            self.max_vertices,
            self.max_whitewater,
        )
        .map_err(|error| format!("Water cache frame {tick} could not be read: {error}"))?;
        if decoded_tick != tick {
            return Err(format!(
                "Water cache frame name {tick} disagrees with its payload {decoded_tick}"
            ));
        }
        Ok((obstacle, stats))
    }
}

fn frame_path(directory: &Path, tick: u64) -> PathBuf {
    directory.join(format!("tick_{tick:012}.zst"))
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_file_name(format!(
        ".{}.tmp-{}",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ))
}

fn write_manifest_atomic(directory: &Path, settings: FluidSettings) -> Result<(), String> {
    let path = directory.join(MANIFEST);
    let temporary = temporary_path(&path);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("Water cache could not create manifest: {error}"))?;
    let mut writer = BufWriter::new(file);
    write_header(&mut writer, settings)
        .map_err(|error| format!("Water cache manifest could not be written: {error}"))?;
    writer
        .flush()
        .and_then(|_| writer.get_ref().sync_all())
        .map_err(|error| format!("Water cache manifest could not sync: {error}"))?;
    fs::rename(&temporary, &path)
        .map_err(|error| format!("Water cache manifest could not publish atomically: {error}"))
}

fn write_header(writer: &mut impl Write, settings: FluidSettings) -> io::Result<()> {
    writer.write_all(MAGIC)?;
    write_u32(writer, FORMAT_VERSION)?;
    writer.write_all(manifold_fluids::UPSTREAM_REVISION.as_bytes())?;
    write_f64(writer, TICK)?;
    write_settings(writer, settings)
}

fn read_manifest(reader: &mut impl Read, settings: FluidSettings) -> io::Result<()> {
    let mut magic = [0; MAGIC.len()];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid magic"));
    }
    let version = read_u32(reader)?;
    if !matches!(version, LEGACY_FORMAT_VERSION | FORMAT_VERSION) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported format version",
        ));
    }
    let mut revision = [0; 40];
    reader.read_exact(&mut revision)?;
    if revision != manifold_fluids::UPSTREAM_REVISION.as_bytes() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "native engine revision does not match",
        ));
    }
    if read_f64(reader)?.to_bits() != TICK.to_bits() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fixed tick does not match 60 Hz",
        ));
    }
    if read_settings(reader, version == FORMAT_VERSION)? != settings {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "physical settings do not match",
        ));
    }
    Ok(())
}

fn write_settings(writer: &mut impl Write, settings: FluidSettings) -> io::Result<()> {
    write_u32(writer, settings.resolution)?;
    write_f32(writer, settings.domain_size)?;
    write_f32(writer, settings.fill_height)?;
    write_transform_opt(writer, settings.initial_volume)?;
    write_u32(writer, settings.surface_subdivisions)?;
    write_f64(writer, settings.surface.particle_scale)?;
    write_f64(writer, settings.surface.smoothing)?;
    write_u32(writer, settings.surface.smoothing_iterations)?;
    write_bool(writer, settings.whitewater.enabled)?;
    write_u32(writer, settings.whitewater.max_particles)?;
    write_f64(writer, settings.whitewater.wavecrest_rate)?;
    write_f64(writer, settings.whitewater.turbulence_rate)?;
    write_f64(writer, settings.whitewater.min_energy)?;
    write_f64(writer, settings.whitewater.max_energy)?;
    write_bool(writer, settings.apic)?;
    write_u64(writer, settings.max_vertices as u64)?;
    write_f64(writer, settings.liquid.viscosity)?;
    write_f64(writer, settings.liquid.surface_tension)
}

fn read_settings(reader: &mut impl Read, includes_liquid: bool) -> io::Result<FluidSettings> {
    let settings = FluidSettings {
        resolution: read_u32(reader)?,
        domain_size: read_f32(reader)?,
        fill_height: read_f32(reader)?,
        initial_volume: read_transform_opt(reader)?,
        surface_subdivisions: read_u32(reader)?,
        surface: manifold_fluids::SurfaceOptions {
            particle_scale: read_f64(reader)?,
            smoothing: read_f64(reader)?,
            smoothing_iterations: read_u32(reader)?,
        },
        whitewater: manifold_fluids::WhitewaterOptions {
            enabled: read_bool(reader)?,
            max_particles: read_u32(reader)?,
            wavecrest_rate: read_f64(reader)?,
            turbulence_rate: read_f64(reader)?,
            min_energy: read_f64(reader)?,
            max_energy: read_f64(reader)?,
        },
        apic: read_bool(reader)?,
        max_vertices: read_u64(reader)? as usize,
        liquid: if includes_liquid {
            manifold_fluids::LiquidOptions {
                viscosity: read_f64(reader)?,
                surface_tension: read_f64(reader)?,
            }
        } else {
            manifold_fluids::LiquidOptions::default()
        },
    };
    Ok(settings)
}

fn write_frame(
    writer: &mut impl Write,
    tick: u64,
    vertices: &[MeshVertex],
    whitewater: &WhitewaterFrame,
    obstacle: Transform,
    stats: FrameStats,
) -> io::Result<()> {
    write_u64(writer, tick)?;
    write_len(writer, vertices.len(), MAX_VERTICES)?;
    write_float_records(writer, vertices)?;
    write_instances(writer, &whitewater.foam)?;
    write_instances(writer, &whitewater.bubbles)?;
    write_instances(writer, &whitewater.spray)?;
    write_transform(writer, obstacle)?;
    write_u32(writer, stats.particles)?;
    write_u32(writer, stats.triangles)?;
    write_u32(writer, stats.substeps)?;
    write_f64(writer, stats.simulation_ms)?;
    write_f64(writer, stats.meshing_ms)
}

fn read_frame(
    reader: &mut impl Read,
    vertices: &mut Vec<MeshVertex>,
    whitewater: &mut WhitewaterFrame,
    max_vertices: usize,
    max_whitewater: usize,
) -> io::Result<(u64, Transform, FrameStats)> {
    let tick = read_u64(reader)?;
    let vertex_count = read_len(reader, max_vertices.min(MAX_VERTICES))?;
    if !vertex_count.is_multiple_of(3) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mesh vertex count is not triangular",
        ));
    }
    read_float_records(reader, vertices, vertex_count)?;
    read_instances(reader, &mut whitewater.foam, max_whitewater)?;
    read_instances(reader, &mut whitewater.bubbles, max_whitewater)?;
    read_instances(reader, &mut whitewater.spray, max_whitewater)?;
    let obstacle = read_transform(reader)?;
    let stats = FrameStats {
        particles: read_u32(reader)?,
        triangles: read_u32(reader)?,
        substeps: read_u32(reader)?,
        simulation_ms: read_f64(reader)?,
        meshing_ms: read_f64(reader)?,
    };
    if whitewater.foam.len() + whitewater.bubbles.len() + whitewater.spray.len() > max_whitewater
        || obstacle
            .pos
            .iter()
            .chain(&obstacle.rot_euler)
            .chain(&obstacle.scale)
            .any(|v| !v.is_finite())
        || !stats.simulation_ms.is_finite()
        || !stats.meshing_ms.is_finite()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid frame metadata or particle count",
        ));
    }
    let mut trailing = [0; 1];
    if reader.read(&mut trailing)? != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "trailing bytes"));
    }
    Ok((tick, obstacle, stats))
}

fn write_instances(writer: &mut impl Write, values: &[InstanceTransform]) -> io::Result<()> {
    write_len(writer, values.len(), MAX_WHITEWATER)?;
    write_float_records(writer, values)
}

fn read_instances(
    reader: &mut impl Read,
    values: &mut Vec<InstanceTransform>,
    max_whitewater: usize,
) -> io::Result<()> {
    let count = read_len(reader, max_whitewater.min(MAX_WHITEWATER))?;
    read_float_records(reader, values, count)
}

fn write_float_records<T: bytemuck::Pod>(writer: &mut impl Write, values: &[T]) -> io::Result<()> {
    let bytes = bytemuck::cast_slice(values);
    if cfg!(target_endian = "little") {
        writer.write_all(bytes)
    } else {
        for word in bytes.chunks_exact(4) {
            writer.write_all(&u32::from_ne_bytes(word.try_into().unwrap()).to_le_bytes())?;
        }
        Ok(())
    }
}

fn read_float_records<T: bytemuck::Pod>(
    reader: &mut impl Read,
    values: &mut Vec<T>,
    count: usize,
) -> io::Result<()> {
    values.resize(count, T::zeroed());
    let bytes = bytemuck::cast_slice_mut(values);
    reader.read_exact(bytes)?;
    for word in bytes.chunks_exact_mut(4) {
        let value = f32::from_le_bytes(word.try_into().unwrap());
        if !value.is_finite() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "non-finite frame data",
            ));
        }
        word.copy_from_slice(&value.to_ne_bytes());
    }
    Ok(())
}

fn write_transform_opt(writer: &mut impl Write, transform: Option<Transform>) -> io::Result<()> {
    write_bool(writer, transform.is_some())?;
    if let Some(value) = transform {
        write_transform(writer, value)?;
    }
    Ok(())
}

fn read_transform_opt(reader: &mut impl Read) -> io::Result<Option<Transform>> {
    Ok(if read_bool(reader)? {
        Some(read_transform(reader)?)
    } else {
        None
    })
}

fn write_transform(writer: &mut impl Write, transform: Transform) -> io::Result<()> {
    for value in transform.pos {
        write_f32(writer, value)?;
    }
    for value in transform.rot_euler {
        write_f32(writer, value)?;
    }
    for value in transform.scale {
        write_f32(writer, value)?;
    }
    write_bool(writer, transform.billboard)
}

fn read_transform(reader: &mut impl Read) -> io::Result<Transform> {
    Ok(Transform {
        pos: [read_f32(reader)?, read_f32(reader)?, read_f32(reader)?],
        rot_euler: [read_f32(reader)?, read_f32(reader)?, read_f32(reader)?],
        scale: [read_f32(reader)?, read_f32(reader)?, read_f32(reader)?],
        billboard: read_bool(reader)?,
    })
}

fn write_len(writer: &mut impl Write, value: usize, max: usize) -> io::Result<()> {
    if value > max || value > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame length exceeds cache limit",
        ));
    }
    write_u32(writer, value as u32)
}

fn read_len(reader: &mut impl Read, max: usize) -> io::Result<usize> {
    let value = read_u32(reader)? as usize;
    if value > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame length exceeds cache limit",
        ));
    }
    Ok(value)
}

fn write_bool(writer: &mut impl Write, value: bool) -> io::Result<()> {
    writer.write_all(&[value as u8])
}
fn read_bool(reader: &mut impl Read) -> io::Result<bool> {
    match read_u8(reader)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid boolean",
        )),
    }
}
fn read_u8(reader: &mut impl Read) -> io::Result<u8> {
    let mut v = [0];
    reader.read_exact(&mut v)?;
    Ok(v[0])
}
fn write_u32(writer: &mut impl Write, value: u32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}
fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut v = [0; 4];
    reader.read_exact(&mut v)?;
    Ok(u32::from_le_bytes(v))
}
fn write_u64(writer: &mut impl Write, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}
fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut v = [0; 8];
    reader.read_exact(&mut v)?;
    Ok(u64::from_le_bytes(v))
}
fn write_f32(writer: &mut impl Write, value: f32) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}
fn read_f32(reader: &mut impl Read) -> io::Result<f32> {
    let mut v = [0; 4];
    reader.read_exact(&mut v)?;
    Ok(f32::from_le_bytes(v))
}
fn write_f64(writer: &mut impl Write, value: f64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}
fn read_f64(reader: &mut impl Read) -> io::Result<f64> {
    let mut v = [0; 8];
    reader.read_exact(&mut v)?;
    Ok(f64::from_le_bytes(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> (Vec<MeshVertex>, WhitewaterFrame, Transform, FrameStats) {
        (
            vec![
                MeshVertex {
                    position: [1.0, 2.0, 3.0],
                    _pad0: 0.0,
                    normal: [0.0, 1.0, 0.0],
                    _pad1: 0.0,
                    uv: [0.25, 0.5],
                    _pad2: [0.0; 2],
                    tangent: [1.0, 0.0, 0.0, 1.0],
                };
                3
            ],
            WhitewaterFrame {
                foam: vec![InstanceTransform {
                    pos_scale: [1.0, 2.0, 3.0, 4.0],
                    rot_pad: [0.0; 4],
                }],
                bubbles: vec![InstanceTransform {
                    pos_scale: [4.0, 3.0, 2.0, 1.0],
                    rot_pad: [1.0; 4],
                }],
                spray: vec![InstanceTransform {
                    pos_scale: [0.0; 4],
                    rot_pad: [2.0; 4],
                }],
            },
            Transform {
                pos: [1.0, 2.0, 3.0],
                rot_euler: [0.1, 0.2, 0.3],
                scale: [2.0, 3.0, 4.0],
                billboard: false,
            },
            FrameStats {
                particles: 3,
                triangles: 1,
                substeps: 2,
                simulation_ms: 1.25,
                meshing_ms: 2.5,
            },
        )
    }

    // Fixed bytes from the pre-LiquidOptions v3 manifest layout at 72e5d2cf6;
    // this fixture must stay independent of the current v4 writer.
    const LEGACY_V3_DEFAULT_MANIFEST: &[u8] = &[
        0x4d, 0x46, 0x4c, 0x55, 0x49, 0x44, 0x43, 0x31, 0x03, 0x00, 0x00, 0x00, 0x37, 0x30, 0x61,
        0x30, 0x65, 0x39, 0x35, 0x34, 0x30, 0x31, 0x38, 0x66, 0x65, 0x33, 0x39, 0x65, 0x31, 0x66,
        0x39, 0x63, 0x33, 0x36, 0x33, 0x31, 0x32, 0x36, 0x34, 0x39, 0x38, 0x39, 0x35, 0x36, 0x39,
        0x37, 0x35, 0x32, 0x62, 0x62, 0x37, 0x61, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x91, 0x3f,
        0x18, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x40, 0xcd, 0xcc, 0xcc, 0x3e, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0xe0, 0x3f, 0x02, 0x00, 0x00, 0x00, 0x00, 0xa0, 0x86, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0xe0, 0x65, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x65, 0x40, 0x9a, 0x99,
        0x99, 0x99, 0x99, 0x99, 0xb9, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4e, 0x40, 0x00,
        0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    fn write_v3_manifest(directory: &Path) {
        fs::create_dir_all(directory).unwrap();
        fs::write(directory.join(MANIFEST), LEGACY_V3_DEFAULT_MANIFEST).unwrap();
    }

    #[test]
    fn cache_round_trip_preserves_all_public_frame_state_and_seeks() {
        let root =
            std::env::temp_dir().join(format!("manifold-fluid-cache-{}", std::process::id()));
        let settings = FluidSettings {
            liquid: manifold_fluids::LiquidOptions {
                viscosity: 0.25,
                surface_tension: 0.1,
            },
            ..FluidSettings::default()
        };
        let (vertices, whitewater, obstacle, stats) = frame();
        let directory = Arc::new(root.join("frames"));
        let writer = CacheWriter::create(directory.clone(), settings).unwrap();
        writer
            .append(2, &vertices, &whitewater, obstacle, stats)
            .unwrap();
        writer
            .append(7, &vertices, &whitewater, obstacle, stats)
            .unwrap();
        let reader = CacheReader::open(directory, settings).unwrap();
        let mut decoded_vertices = Vec::new();
        let mut decoded_whitewater = WhitewaterFrame::default();
        let (decoded_obstacle, decoded_stats) = reader
            .read_into(7, &mut decoded_vertices, &mut decoded_whitewater)
            .unwrap();
        assert_eq!(
            bytemuck::cast_slice::<_, u8>(&decoded_vertices),
            bytemuck::cast_slice::<_, u8>(&vertices)
        );
        reader
            .read_into(2, &mut decoded_vertices, &mut decoded_whitewater)
            .unwrap();
        assert_eq!(
            bytemuck::cast_slice::<_, u8>(&decoded_vertices),
            bytemuck::cast_slice::<_, u8>(&vertices)
        );
        assert_eq!(
            decoded_whitewater.foam[0].pos_scale,
            whitewater.foam[0].pos_scale
        );
        assert_eq!(
            decoded_whitewater.bubbles[0].rot_pad,
            whitewater.bubbles[0].rot_pad
        );
        assert_eq!(
            decoded_whitewater.spray[0].rot_pad,
            whitewater.spray[0].rot_pad
        );
        assert_eq!(decoded_obstacle, obstacle);
        assert_eq!(decoded_stats, stats);
        assert!(
            reader
                .read_into(3, &mut decoded_vertices, &mut decoded_whitewater)
                .is_err()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cache_manifest_rejects_physical_settings_mismatch() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-mismatch-{}",
            std::process::id()
        ));
        let directory = Arc::new(root.join("frames"));
        let settings = FluidSettings::default();
        let _writer = CacheWriter::create(directory.clone(), settings).unwrap();
        let mut changed = settings;
        changed.apic = true;
        assert!(CacheReader::open(directory, changed).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cache_v4_rejects_each_liquid_coefficient_mismatch() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-liquid-mismatch-{}",
            std::process::id()
        ));
        let directory = Arc::new(root.join("frames"));
        let settings = FluidSettings {
            liquid: manifold_fluids::LiquidOptions {
                viscosity: 0.25,
                surface_tension: 0.1,
            },
            ..FluidSettings::default()
        };
        let _writer = CacheWriter::create(directory.clone(), settings).unwrap();
        assert!(CacheReader::open(directory.clone(), settings).is_ok());

        let mut changed_viscosity = settings;
        changed_viscosity.liquid.viscosity = 0.5;
        assert!(CacheReader::open(directory.clone(), changed_viscosity).is_err());

        let mut changed_surface_tension = settings;
        changed_surface_tension.liquid.surface_tension = 0.2;
        assert!(CacheReader::open(directory, changed_surface_tension).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_v3_defaults_liquid_options_and_rejects_nonzero_requests() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-v3-liquid-{}",
            std::process::id()
        ));
        let directory = root.join("frames");
        let settings = FluidSettings::default();
        write_v3_manifest(&directory);
        assert!(CacheReader::open(Arc::new(directory.clone()), settings).is_ok());

        let mut changed = settings;
        changed.liquid.viscosity = 0.25;
        assert!(CacheReader::open(Arc::new(directory.clone()), changed).is_err());
        changed.liquid = manifold_fluids::LiquidOptions {
            viscosity: 0.0,
            surface_tension: 0.1,
        };
        assert!(CacheReader::open(Arc::new(directory), changed).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_rejects_corrupt_truncated_oversized_frames_and_directory_reuse() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-corrupt-{}",
            std::process::id()
        ));
        let directory = Arc::new(root.clone());
        let settings = FluidSettings::default();
        let writer = CacheWriter::create(directory.clone(), settings).unwrap();
        assert!(CacheWriter::create(directory.clone(), settings).is_err());
        let (vertices, whitewater, obstacle, stats) = frame();
        writer
            .append(1, &vertices, &whitewater, obstacle, stats)
            .unwrap();
        let reader = CacheReader::open(directory, settings).unwrap();
        let path = frame_path(&root, 1);
        let original = fs::read(&path).unwrap();
        let mut corrupt = original.clone();
        *corrupt.last_mut().unwrap() ^= 1; // checksum must detect even a valid payload
        fs::write(&path, &corrupt).unwrap();
        let mut decoded = Vec::new();
        let mut ww = WhitewaterFrame::default();
        assert!(reader.read_into(1, &mut decoded, &mut ww).is_err());
        fs::write(&path, &original[..original.len() / 2]).unwrap();
        assert!(reader.read_into(1, &mut decoded, &mut ww).is_err());
        let mut oversized = Vec::new();
        write_u64(&mut oversized, 1).unwrap();
        write_u64(&mut oversized, MAX_VERTICES as u64 + 1).unwrap();
        fs::write(
            &path,
            zstd::stream::encode_all(oversized.as_slice(), 1).unwrap(),
        )
        .unwrap();
        decoded.clear();
        let capacity = decoded.capacity();
        assert!(reader.read_into(1, &mut decoded, &mut ww).is_err());
        assert_eq!(decoded.capacity(), capacity);
        fs::remove_dir_all(root).unwrap();
    }
}
