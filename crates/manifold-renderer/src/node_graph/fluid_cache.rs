//! Bounded-memory, compressed baked fluid snapshots.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::fluid::{
    CoupledRigidFrame, FluidSettings, FluidTakeIdentity, FluidTakeReplay, PlaybackClock, TICK,
    simulation_tick,
};
use super::transform::Transform;
use crate::generators::mesh_common::{InstanceTransform, MeshVertex};
use crate::node_graph::fluid::WhitewaterFrame;
use crate::node_graph::physics::{MAX_BODIES, MAX_COPIES};
use manifold_fluids::FrameStats;

const MAGIC: &[u8; 8] = b"MFLUIDC1";
const LEGACY_FORMAT_VERSION: u32 = 3;
const LEGACY_LIQUID_FORMAT_VERSION: u32 = 4;
const LEGACY_TIME_STEPS_FORMAT_VERSION: u32 = 5;
const MESH_VERTEX_FORMAT_VERSION: u32 = 6;
const DOMAIN_FORMAT_VERSION: u32 = 7;
const PAIRED_FORMAT_VERSION: u32 = 8;
const LEGACY_SOURCE_IDENTITY_FORMAT_VERSION: u32 = 9;
const FORMAT_VERSION: u32 = 10;
const TAKE_TICK_LIMIT: u64 = (1 << 53) - 1;
const BINDING_UNBOUND: u8 = 0;
const BINDING_PENDING: u8 = 1;
const BINDING_BOUND: u8 = 2;
const LEGACY_MESH_VERTEX_SIZE: usize = 64;
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
    settings: FluidSettings,
    max_vertices: usize,
    max_whitewater: usize,
    binding: CacheManifestBinding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CacheManifestBinding {
    Unbound,
    Pending,
    Bound(FluidTakeIdentity),
}

impl CacheWriter {
    #[cfg(test)]
    pub(crate) fn create(directory: Arc<PathBuf>, settings: FluidSettings) -> Result<Self, String> {
        Self::create_inner(directory, settings, CacheManifestBinding::Unbound)
    }

    pub(crate) fn create_for_take(
        directory: Arc<PathBuf>,
        settings: FluidSettings,
    ) -> Result<Self, String> {
        Self::create_inner(directory, settings, CacheManifestBinding::Pending)
    }

    fn create_inner(
        directory: Arc<PathBuf>,
        settings: FluidSettings,
        binding: CacheManifestBinding,
    ) -> Result<Self, String> {
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
        write_manifest_atomic(directory.as_ref(), settings, binding)?;
        Ok(Self {
            directory,
            settings,
            max_vertices: settings.max_vertices,
            max_whitewater: settings.whitewater.max_particles as usize,
            binding,
        })
    }

    pub(crate) fn publish_take_prefix(
        &mut self,
        identity: FluidTakeIdentity,
    ) -> Result<(), String> {
        validate_take_identity(identity)
            .map_err(|error| format!("Water cache take prefix identity is invalid: {error}"))?;
        if self.binding == CacheManifestBinding::Unbound {
            return Err("Water cache writer is not bound to a take".into());
        }
        if let CacheManifestBinding::Bound(previous) = self.binding {
            if identity.setup != previous.setup {
                return Err("Water cache take prefix identity changed after publication".into());
            }
            if identity.completed_tick < previous.completed_tick {
                return Err("Water cache take prefix regressed after publication".into());
            }
        }
        write_manifest_atomic(
            self.directory.as_ref(),
            self.settings,
            CacheManifestBinding::Bound(identity),
        )?;
        self.binding = CacheManifestBinding::Bound(identity);
        Ok(())
    }

    pub(crate) fn append(
        &self,
        tick: u64,
        vertices: &[MeshVertex],
        whitewater: &WhitewaterFrame,
        obstacle: Transform,
        stats: FrameStats,
    ) -> Result<(), String> {
        self.append_paired(tick, vertices, whitewater, obstacle, stats, None)
    }

    pub(crate) fn append_paired(
        &self,
        tick: u64,
        vertices: &[MeshVertex],
        whitewater: &WhitewaterFrame,
        obstacle: Transform,
        stats: FrameStats,
        rigid: Option<&CoupledRigidFrame>,
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
        write_frame(
            &mut encoder,
            tick,
            vertices,
            whitewater,
            obstacle,
            stats,
            rigid,
        )
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
    format_version: u32,
    take_identity: Option<FluidTakeIdentity>,
    project_clock: Option<PlaybackClock>,
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
        let (format_version, binding) = read_manifest(&mut reader, settings)
            .map_err(|error| format!("Water cache playback rejected manifest: {error}"))?;
        let (take_identity, project_clock) = match binding {
            CacheManifestBinding::Unbound => (None, None),
            CacheManifestBinding::Pending => {
                return Err("Water cache playback rejected pending take prefix".into());
            }
            CacheManifestBinding::Bound(identity) => {
                let replay = FluidTakeReplay::open(directory.as_ref()).map_err(|error| {
                    format!("Water cache playback could not validate its committed take: {error}")
                })?;
                if replay.settings() != settings {
                    return Err(
                        "Water cache playback take settings do not match its committed take".into(),
                    );
                }
                if replay.identity() != identity {
                    return Err(
                        "Water cache playback take identity does not match its committed take"
                            .into(),
                    );
                }
                (Some(identity), replay.into_playback_clock())
            }
        };
        Ok(Self {
            directory,
            format_version,
            take_identity,
            project_clock,
            max_vertices: settings.max_vertices,
            max_whitewater: settings.whitewater.max_particles as usize,
        })
    }

    /// Resolve only on the worker. Timed takes retain their recorded speed
    /// changes and holds; legacy caches explicitly keep absolute tick lookup.
    pub(crate) fn playback_tick(
        &mut self,
        project_time: Option<manifold_core::Seconds>,
        legacy_tick: u64,
    ) -> Result<u64, String> {
        if let Some(clock) = &mut self.project_clock {
            let project_time = project_time
                .ok_or("Water cache playback requires the recorded project-time address")?;
            let simulation = clock.simulation_time_at(project_time)?;
            Ok(simulation_tick(simulation.0))
        } else {
            Ok(legacy_tick)
        }
    }

    pub(crate) fn read_into(
        &self,
        tick: u64,
        vertices: &mut Vec<MeshVertex>,
        whitewater: &mut WhitewaterFrame,
    ) -> Result<(Transform, FrameStats), String> {
        let (obstacle, stats, paired) = self.read_paired_into(tick, vertices, whitewater, None)?;
        if paired {
            return Err(format!(
                "Water cache frame {tick} contains paired rigid poses; use read_paired_into"
            ));
        }
        Ok((obstacle, stats))
    }

    pub(crate) fn read_paired_into(
        &self,
        tick: u64,
        vertices: &mut Vec<MeshVertex>,
        whitewater: &mut WhitewaterFrame,
        rigid: Option<&mut CoupledRigidFrame>,
    ) -> Result<(Transform, FrameStats, bool), String> {
        if let Some(identity) = self.take_identity()
            && tick > identity.completed_tick
        {
            return Err(format!(
                "Water cache frame tick {tick} exceeds committed take prefix {}",
                identity.completed_tick
            ));
        }
        let path = frame_path(self.directory.as_ref(), tick);
        let file = File::open(&path)
            .map_err(|error| format!("Water cache has no baked frame for tick {tick}: {error}"))?;
        let mut decoder = zstd::stream::read::Decoder::new(BufReader::new(file))
            .map_err(|error| format!("Water cache frame {tick} could not decompress: {error}"))?;
        decoder
            .window_log_max(23)
            .map_err(|error| format!("Water cache decompression limit failed: {error}"))?;
        let (decoded_tick, obstacle, stats, paired) = read_frame(
            &mut decoder,
            vertices,
            whitewater,
            self.format_version,
            self.max_vertices,
            self.max_whitewater,
            rigid,
        )
        .map_err(|error| format!("Water cache frame {tick} could not be read: {error}"))?;
        if decoded_tick != tick {
            return Err(format!(
                "Water cache frame name {tick} disagrees with its payload {decoded_tick}"
            ));
        }
        Ok((obstacle, stats, paired))
    }

    pub(crate) fn take_identity(&self) -> Option<FluidTakeIdentity> {
        self.take_identity
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

fn write_manifest_atomic(
    directory: &Path,
    settings: FluidSettings,
    binding: CacheManifestBinding,
) -> Result<(), String> {
    let path = directory.join(MANIFEST);
    let temporary = temporary_path(&path);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("Water cache could not create manifest: {error}"))?;
    let mut writer = BufWriter::new(file);
    write_header(&mut writer, settings, binding)
        .map_err(|error| format!("Water cache manifest could not be written: {error}"))?;
    writer
        .flush()
        .and_then(|_| writer.get_ref().sync_all())
        .map_err(|error| format!("Water cache manifest could not sync: {error}"))?;
    fs::rename(&temporary, &path)
        .map_err(|error| format!("Water cache manifest could not publish atomically: {error}"))
}

fn write_header(
    writer: &mut impl Write,
    settings: FluidSettings,
    binding: CacheManifestBinding,
) -> io::Result<()> {
    writer.write_all(MAGIC)?;
    write_u32(writer, FORMAT_VERSION)?;
    writer.write_all(manifold_fluids::UPSTREAM_REVISION.as_bytes())?;
    write_f64(writer, TICK)?;
    writer.write_all(&super::fluid::identity::solver_identity())?;
    write_settings(writer, settings)?;
    write_u64(writer, settings.seed)?;
    match binding {
        CacheManifestBinding::Unbound => write_u8(writer, BINDING_UNBOUND),
        CacheManifestBinding::Pending => write_u8(writer, BINDING_PENDING),
        CacheManifestBinding::Bound(identity) => {
            validate_take_identity(identity)?;
            write_u8(writer, BINDING_BOUND)?;
            writer.write_all(&identity.setup)?;
            writer.write_all(&identity.inputs)?;
            write_u64(writer, identity.completed_tick)
        }
    }
}

fn read_manifest(
    reader: &mut impl Read,
    settings: FluidSettings,
) -> io::Result<(u32, CacheManifestBinding)> {
    let mut magic = [0; MAGIC.len()];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid magic"));
    }
    let version = read_u32(reader)?;
    if !matches!(
        version,
        LEGACY_FORMAT_VERSION
            | LEGACY_LIQUID_FORMAT_VERSION
            | LEGACY_TIME_STEPS_FORMAT_VERSION
            | MESH_VERTEX_FORMAT_VERSION
            | DOMAIN_FORMAT_VERSION
            | PAIRED_FORMAT_VERSION
            | LEGACY_SOURCE_IDENTITY_FORMAT_VERSION
            | FORMAT_VERSION
    ) {
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
    if version >= LEGACY_SOURCE_IDENTITY_FORMAT_VERSION {
        let mut identity = [0; 32];
        reader.read_exact(&mut identity)?;
        if identity != super::fluid::identity::solver_identity() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "physics solver or adapter sources do not match",
            ));
        }
    }
    if read_settings(
        reader,
        version >= LEGACY_LIQUID_FORMAT_VERSION,
        version >= LEGACY_TIME_STEPS_FORMAT_VERSION,
        version >= DOMAIN_FORMAT_VERSION,
        version >= LEGACY_SOURCE_IDENTITY_FORMAT_VERSION,
    )? != settings
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "physical settings do not match",
        ));
    }
    if version < FORMAT_VERSION {
        return Ok((version, CacheManifestBinding::Unbound));
    }
    let binding = match read_u8(reader)? {
        BINDING_UNBOUND => CacheManifestBinding::Unbound,
        BINDING_PENDING => CacheManifestBinding::Pending,
        BINDING_BOUND => {
            let mut setup = [0; 32];
            let mut inputs = [0; 32];
            reader.read_exact(&mut setup)?;
            reader.read_exact(&mut inputs)?;
            let identity = FluidTakeIdentity {
                setup,
                inputs,
                completed_tick: read_u64(reader)?,
            };
            validate_take_identity(identity)?;
            CacheManifestBinding::Bound(identity)
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid take binding state",
            ));
        }
    };
    let mut trailing = [0; 1];
    if reader.read(&mut trailing)? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing manifest bytes",
        ));
    }
    Ok((version, binding))
}

fn validate_take_identity(identity: FluidTakeIdentity) -> io::Result<()> {
    if identity.completed_tick > TAKE_TICK_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "committed take tick exceeds exact integer range",
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
    write_f64(writer, settings.liquid.surface_tension)?;
    write_u32(writer, settings.time_steps.min_substeps)?;
    write_u32(writer, settings.time_steps.max_substeps)?;
    write_u32(writer, settings.time_steps.cfl)?;
    write_bool(writer, settings.time_steps.adaptive_obstacles)?;
    write_transform_opt(writer, settings.domain)?;
    for face in settings.boundary_collisions {
        write_bool(writer, face)?;
    }
    Ok(())
}

fn read_settings(
    reader: &mut impl Read,
    includes_liquid: bool,
    includes_time_steps: bool,
    includes_domain: bool,
    includes_seed: bool,
) -> io::Result<FluidSettings> {
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
            // v3 predates liquid coefficients; its historical values are fixed.
            manifold_fluids::LiquidOptions {
                viscosity: 0.0,
                surface_tension: 0.0,
            }
        },
        time_steps: if includes_time_steps {
            manifold_fluids::TimeStepOptions {
                min_substeps: read_u32(reader)?,
                max_substeps: read_u32(reader)?,
                cfl: read_u32(reader)?,
                adaptive_obstacles: read_bool(reader)?,
            }
        } else {
            // Preserve v3/v4 physics identity even if future runtime defaults change.
            manifold_fluids::TimeStepOptions {
                min_substeps: 1,
                max_substeps: 6,
                cfl: 5,
                adaptive_obstacles: false,
            }
        },
        domain: if includes_domain {
            read_transform_opt(reader)?
        } else {
            None
        },
        boundary_collisions: if includes_domain {
            [
                read_bool(reader)?,
                read_bool(reader)?,
                read_bool(reader)?,
                read_bool(reader)?,
                read_bool(reader)?,
                read_bool(reader)?,
            ]
        } else {
            [true; 6]
        },
        seed: if includes_seed {
            read_u64(reader)?
        } else {
            manifold_fluids::DEFAULT_SEED
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
    rigid: Option<&CoupledRigidFrame>,
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
    write_f64(writer, stats.meshing_ms)?;
    write_bool(writer, rigid.is_some())?;
    if let Some(rigid) = rigid {
        write_rigid_frame(writer, tick, rigid)?;
    }
    Ok(())
}

fn read_frame(
    reader: &mut impl Read,
    vertices: &mut Vec<MeshVertex>,
    whitewater: &mut WhitewaterFrame,
    format_version: u32,
    max_vertices: usize,
    max_whitewater: usize,
    rigid: Option<&mut CoupledRigidFrame>,
) -> io::Result<(u64, Transform, FrameStats, bool)> {
    let tick = read_u64(reader)?;
    let vertex_count = read_len(reader, max_vertices.min(MAX_VERTICES))?;
    if !vertex_count.is_multiple_of(3) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mesh vertex count is not triangular",
        ));
    }
    if format_version < MESH_VERTEX_FORMAT_VERSION {
        read_legacy_mesh_vertices(reader, vertices, vertex_count)?;
    } else {
        read_float_records(reader, vertices, vertex_count)?;
    }
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
    let paired = if format_version >= PAIRED_FORMAT_VERSION {
        if read_bool(reader)? {
            read_rigid_frame(reader, tick, rigid)?;
            true
        } else {
            false
        }
    } else {
        false
    };
    let mut trailing = [0; 1];
    if reader.read(&mut trailing)? != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "trailing bytes"));
    }
    Ok((tick, obstacle, stats, paired))
}

fn write_rigid_frame(
    writer: &mut impl Write,
    tick: u64,
    rigid: &CoupledRigidFrame,
) -> io::Result<()> {
    validate_rigid_frame(tick, rigid, io::ErrorKind::InvalidInput)?;
    write_u64(writer, rigid.stamp.epoch)?;
    write_u64(writer, rigid.stamp.tick)?;
    for pose in rigid.poses {
        write_transform(writer, pose)?;
    }
    write_len(writer, rigid.copies.len(), MAX_COPIES)?;
    for copy in &rigid.copies {
        write_transform(writer, *copy)?;
    }
    Ok(())
}

fn read_rigid_frame(
    reader: &mut impl Read,
    tick: u64,
    rigid: Option<&mut CoupledRigidFrame>,
) -> io::Result<()> {
    let epoch = read_u64(reader)?;
    let stored_tick = read_u64(reader)?;
    if stored_tick != tick {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "paired rigid tick does not match fluid tick",
        ));
    }
    let mut rigid = rigid;
    for index in 0..MAX_BODIES {
        let pose = read_transform(reader)?;
        if !transform_is_finite(pose) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "non-finite paired rigid pose",
            ));
        }
        if let Some(output) = rigid.as_deref_mut() {
            output.poses[index] = pose;
        }
    }
    let copy_count = read_len(reader, MAX_COPIES)?;
    if let Some(output) = rigid {
        output.copies.resize(copy_count, Transform::default());
        for copy in &mut output.copies {
            *copy = read_transform(reader)?;
            if !transform_is_finite(*copy) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "non-finite paired rigid copy",
                ));
            }
        }
        output.stamp = manifold_physics::TickStamp { epoch, tick };
    } else {
        for _ in 0..copy_count {
            if !transform_is_finite(read_transform(reader)?) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "non-finite paired rigid copy",
                ));
            }
        }
    }
    Ok(())
}

fn validate_rigid_frame(
    tick: u64,
    rigid: &CoupledRigidFrame,
    kind: io::ErrorKind,
) -> io::Result<()> {
    if rigid.stamp.tick != tick {
        return Err(io::Error::new(
            kind,
            "paired rigid tick does not match fluid tick",
        ));
    }
    if rigid.copies.len() > MAX_COPIES {
        return Err(io::Error::new(
            kind,
            "paired rigid copy count exceeds physics capacity",
        ));
    }
    if rigid
        .poses
        .iter()
        .copied()
        .any(|pose| !transform_is_finite(pose))
        || rigid
            .copies
            .iter()
            .copied()
            .any(|pose| !transform_is_finite(pose))
    {
        return Err(io::Error::new(kind, "non-finite paired rigid pose"));
    }
    Ok(())
}

fn transform_is_finite(transform: Transform) -> bool {
    transform
        .pos
        .iter()
        .chain(&transform.rot_euler)
        .chain(&transform.scale)
        .all(|value| value.is_finite())
}

fn read_legacy_mesh_vertices(
    reader: &mut impl Read,
    vertices: &mut Vec<MeshVertex>,
    count: usize,
) -> io::Result<()> {
    let zero = MeshVertex {
        position: [0.0; 3],
        _pad0: 0.0,
        normal: [0.0; 3],
        _pad1: 0.0,
        uv: [0.0; 2],
        _pad2: [0.0; 2],
        tangent: [0.0; 4],
        color: [1.0; 4],
    };
    vertices.resize(count, zero);
    for vertex in vertices.iter_mut() {
        let mut bytes = [0; LEGACY_MESH_VERTEX_SIZE];
        reader.read_exact(&mut bytes)?;
        let mut words = [0.0; LEGACY_MESH_VERTEX_SIZE / std::mem::size_of::<f32>()];
        for (word, bytes) in words.iter_mut().zip(bytes.chunks_exact(4)) {
            *word = f32::from_le_bytes(bytes.try_into().unwrap());
            if !word.is_finite() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "non-finite frame data",
                ));
            }
        }
        *vertex = MeshVertex {
            position: [words[0], words[1], words[2]],
            _pad0: words[3],
            normal: [words[4], words[5], words[6]],
            _pad1: words[7],
            uv: [words[8], words[9]],
            _pad2: [words[10], words[11]],
            tangent: [words[12], words[13], words[14], words[15]],
            color: [1.0; 4],
        };
    }
    Ok(())
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
fn write_u8(writer: &mut impl Write, value: u8) -> io::Result<()> {
    writer.write_all(&[value])
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
                    color: [0.2, 0.4, 0.6, 1.0],
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

    fn take_identity(completed_tick: u64) -> FluidTakeIdentity {
        FluidTakeIdentity {
            setup: [0x11; 32],
            inputs: [0x22; 32],
            completed_tick,
        }
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

    // Fixed bytes from the committed pre-TimeStepOptions v4 manifest layout
    // at bbbbdb060; this fixture must stay independent of the current v5 writer.
    const LEGACY_V4_LIQUID_MANIFEST: &[u8] = &[
        0x4d, 0x46, 0x4c, 0x55, 0x49, 0x44, 0x43, 0x31, 0x04, 0x00, 0x00, 0x00, 0x37, 0x30, 0x61,
        0x30, 0x65, 0x39, 0x35, 0x34, 0x30, 0x31, 0x38, 0x66, 0x65, 0x33, 0x39, 0x65, 0x31, 0x66,
        0x39, 0x63, 0x33, 0x36, 0x33, 0x31, 0x32, 0x36, 0x34, 0x39, 0x38, 0x39, 0x35, 0x36, 0x39,
        0x37, 0x35, 0x32, 0x62, 0x62, 0x37, 0x61, 0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x91, 0x3f,
        0x18, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x40, 0xcd, 0xcc, 0xcc, 0x3e, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0xe0, 0x3f, 0x02, 0x00, 0x00, 0x00, 0x00, 0xa0, 0x86, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0xe0, 0x65, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x65, 0x40, 0x9a, 0x99,
        0x99, 0x99, 0x99, 0x99, 0xb9, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x4e, 0x40, 0x00,
        0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xd0,
        0x3f, 0x9a, 0x99, 0x99, 0x99, 0x99, 0x99, 0xb9, 0x3f,
    ];

    fn write_v3_manifest(directory: &Path) {
        fs::create_dir_all(directory).unwrap();
        fs::write(directory.join(MANIFEST), LEGACY_V3_DEFAULT_MANIFEST).unwrap();
    }

    fn write_v4_liquid_manifest(directory: &Path) {
        fs::create_dir_all(directory).unwrap();
        fs::write(directory.join(MANIFEST), LEGACY_V4_LIQUID_MANIFEST).unwrap();
    }

    fn write_unpaired_payload(writer: &mut Vec<u8>, tick: u64) {
        let (vertices, whitewater, obstacle, stats) = frame();
        write_u64(writer, tick).unwrap();
        write_len(writer, vertices.len(), MAX_VERTICES).unwrap();
        write_float_records(writer, &vertices).unwrap();
        write_instances(writer, &whitewater.foam).unwrap();
        write_instances(writer, &whitewater.bubbles).unwrap();
        write_instances(writer, &whitewater.spray).unwrap();
        write_transform(writer, obstacle).unwrap();
        write_u32(writer, stats.particles).unwrap();
        write_u32(writer, stats.triangles).unwrap();
        write_u32(writer, stats.substeps).unwrap();
        write_f64(writer, stats.simulation_ms).unwrap();
        write_f64(writer, stats.meshing_ms).unwrap();
    }

    fn write_paired_payload(writer: &mut Vec<u8>, tick: u64, copy_count: u32, nonfinite: bool) {
        write_unpaired_payload(writer, tick);
        write_bool(writer, true).unwrap();
        write_u64(writer, 9).unwrap();
        write_u64(writer, tick).unwrap();
        for index in 0..MAX_BODIES {
            let mut pose = Transform::default();
            if nonfinite && index == 0 {
                pose.pos[0] = f32::NAN;
            }
            write_transform(writer, pose).unwrap();
        }
        write_u32(writer, copy_count).unwrap();
        for _ in 0..copy_count.min(MAX_COPIES as u32) {
            write_transform(writer, Transform::default()).unwrap();
        }
    }

    #[test]
    fn cache_round_trip_preserves_all_public_frame_state_and_seeks() {
        let root =
            std::env::temp_dir().join(format!("manifold-fluid-cache-{}", std::process::id()));
        let settings = FluidSettings {
            domain: Some(Transform {
                pos: [0.25, 0.5, -0.25],
                scale: [4.0, 3.0, 5.0],
                ..Transform::default()
            }),
            boundary_collisions: [true, false, true, false, true, false],
            liquid: manifold_fluids::LiquidOptions {
                viscosity: 0.25,
                surface_tension: 0.1,
            },
            time_steps: manifold_fluids::TimeStepOptions {
                min_substeps: 2,
                max_substeps: 8,
                cfl: 4,
                adaptive_obstacles: true,
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
        assert_eq!(reader.take_identity(), None);
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
    fn cache_take_writer_stays_pending_until_prefix_publication() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-pending-{}",
            std::process::id()
        ));
        let directory = Arc::new(root.join("frames"));
        let _writer =
            CacheWriter::create_for_take(directory.clone(), FluidSettings::default()).unwrap();
        let error = CacheReader::open(directory, FluidSettings::default())
            .err()
            .unwrap();
        assert!(error.contains("pending take prefix"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_v9_keeps_source_identity_without_take_binding() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-v9-legacy-{}",
            std::process::id()
        ));
        let directory = root.join("frames");
        let settings = FluidSettings::default();
        fs::create_dir_all(&directory).unwrap();
        let mut manifest = Vec::new();
        manifest.extend_from_slice(MAGIC);
        write_u32(&mut manifest, LEGACY_SOURCE_IDENTITY_FORMAT_VERSION).unwrap();
        manifest.extend_from_slice(manifold_fluids::UPSTREAM_REVISION.as_bytes());
        write_f64(&mut manifest, TICK).unwrap();
        manifest.extend_from_slice(&super::super::fluid::identity::solver_identity());
        write_settings(&mut manifest, settings).unwrap();
        write_u64(&mut manifest, settings.seed).unwrap();
        fs::write(directory.join(MANIFEST), manifest).unwrap();

        let reader = CacheReader::open(Arc::new(directory), settings).unwrap();
        assert_eq!(reader.take_identity(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_v10_rejects_unknown_and_truncated_binding_payloads() {
        let settings = FluidSettings::default();
        let identity = take_identity(12);
        let mut manifest = Vec::new();
        write_header(
            &mut manifest,
            settings,
            CacheManifestBinding::Bound(identity),
        )
        .unwrap();
        let mut unknown = Vec::new();
        write_header(&mut unknown, settings, CacheManifestBinding::Unbound).unwrap();
        *unknown.last_mut().unwrap() = 9;
        assert!(read_manifest(&mut unknown.as_slice(), settings).is_err());
        for length in [0, manifest.len() - 1, manifest.len() - 8] {
            assert!(read_manifest(&mut &manifest[..length], settings).is_err());
        }
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
    fn cache_settings_identity_rejects_each_physical_change() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-liquid-mismatch-{}",
            std::process::id()
        ));
        let directory = Arc::new(root.join("frames"));
        let settings = FluidSettings {
            seed: 0x3141_5926_5358_9793,
            domain: Some(Transform {
                pos: [0.25, 0.5, -0.25],
                scale: [4.0, 3.0, 5.0],
                ..Transform::default()
            }),
            boundary_collisions: [true, false, true, false, true, false],
            liquid: manifold_fluids::LiquidOptions {
                viscosity: 0.25,
                surface_tension: 0.1,
            },
            time_steps: manifold_fluids::TimeStepOptions {
                min_substeps: 2,
                max_substeps: 8,
                cfl: 4,
                adaptive_obstacles: true,
            },
            ..FluidSettings::default()
        };
        let _writer = CacheWriter::create(directory.clone(), settings).unwrap();
        assert!(CacheReader::open(directory.clone(), settings).is_ok());

        let mut changed_seed = settings;
        changed_seed.seed ^= 1 << 40;
        assert!(CacheReader::open(directory.clone(), changed_seed).is_err());

        let mut changed_viscosity = settings;
        changed_viscosity.liquid.viscosity = 0.5;
        assert!(CacheReader::open(directory.clone(), changed_viscosity).is_err());

        let mut changed_surface_tension = settings;
        changed_surface_tension.liquid.surface_tension = 0.2;
        assert!(CacheReader::open(directory.clone(), changed_surface_tension).is_err());

        let mut changed_min_substeps = settings;
        changed_min_substeps.time_steps.min_substeps = 3;
        assert!(CacheReader::open(directory.clone(), changed_min_substeps).is_err());

        let mut changed_max_substeps = settings;
        changed_max_substeps.time_steps.max_substeps = 9;
        assert!(CacheReader::open(directory.clone(), changed_max_substeps).is_err());

        let mut changed_cfl = settings;
        changed_cfl.time_steps.cfl = 6;
        assert!(CacheReader::open(directory.clone(), changed_cfl).is_err());

        let mut changed_adaptive_obstacles = settings;
        changed_adaptive_obstacles.time_steps.adaptive_obstacles = false;
        assert!(CacheReader::open(directory.clone(), changed_adaptive_obstacles).is_err());

        let mut changed_domain_translation = settings;
        changed_domain_translation.domain.as_mut().unwrap().pos[0] += 0.1;
        assert!(CacheReader::open(directory.clone(), changed_domain_translation).is_err());

        let mut changed_domain_size = settings;
        changed_domain_size.domain.as_mut().unwrap().scale[1] += 0.1;
        assert!(CacheReader::open(directory.clone(), changed_domain_size).is_err());

        let mut changed_boundary = settings;
        changed_boundary.boundary_collisions[0] = false;
        assert!(CacheReader::open(directory, changed_boundary).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_v5_decodes_legacy_64_byte_vertices_with_white_color() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-v5-legacy-{}",
            std::process::id()
        ));
        let directory = root.join("frames");
        let settings = FluidSettings::default();
        fs::create_dir_all(&directory).unwrap();

        let mut manifest = Vec::new();
        manifest.extend_from_slice(MAGIC);
        write_u32(&mut manifest, LEGACY_TIME_STEPS_FORMAT_VERSION).unwrap();
        manifest.extend_from_slice(manifold_fluids::UPSTREAM_REVISION.as_bytes());
        write_f64(&mut manifest, TICK).unwrap();
        write_settings(&mut manifest, settings).unwrap();
        fs::write(directory.join(MANIFEST), manifest).unwrap();

        let mut frame = Vec::new();
        write_u64(&mut frame, 11).unwrap();
        write_u32(&mut frame, 3).unwrap();
        for index in 0..3 {
            let value = index as f32;
            let words = [
                value,
                value + 0.25,
                value + 0.5,
                0.0,
                0.0,
                1.0,
                value,
                0.0,
                value * 0.01,
                value * 0.02,
                0.0,
                0.0,
                1.0,
                0.0,
                0.0,
                1.0,
            ];
            for word in words {
                write_f32(&mut frame, word).unwrap();
            }
        }
        write_u32(&mut frame, 0).unwrap();
        write_u32(&mut frame, 0).unwrap();
        write_u32(&mut frame, 0).unwrap();
        write_transform(
            &mut frame,
            Transform {
                pos: [0.0, 0.0, 0.0],
                rot_euler: [0.0, 0.0, 0.0],
                scale: [1.0, 1.0, 1.0],
                billboard: false,
            },
        )
        .unwrap();
        write_u32(&mut frame, 3).unwrap();
        write_u32(&mut frame, 1).unwrap();
        write_u32(&mut frame, 2).unwrap();
        write_f64(&mut frame, 1.25).unwrap();
        write_f64(&mut frame, 2.5).unwrap();
        let path = frame_path(&directory, 11);
        let encoded = zstd::stream::encode_all(frame.as_slice(), 1).unwrap();
        fs::write(&path, &encoded).unwrap();

        let reader = CacheReader::open(Arc::new(directory.clone()), settings).unwrap();
        let mut changed_domain = settings;
        changed_domain.domain = Some(Transform {
            scale: [4.0, 3.0, 5.0],
            ..Transform::default()
        });
        assert!(CacheReader::open(Arc::new(directory.clone()), changed_domain).is_err());
        let mut changed_boundary = settings;
        changed_boundary.boundary_collisions[0] = false;
        assert!(CacheReader::open(Arc::new(directory.clone()), changed_boundary).is_err());
        let mut vertices = Vec::new();
        let mut whitewater = WhitewaterFrame::default();
        reader
            .read_into(11, &mut vertices, &mut whitewater)
            .unwrap();
        assert_eq!(vertices.len(), 3);
        for (index, vertex) in vertices.iter().enumerate() {
            let value = index as f32;
            assert_eq!(vertex.position, [value, value + 0.25, value + 0.5]);
            assert_eq!(vertex.tangent, [1.0, 0.0, 0.0, 1.0]);
            assert_eq!(vertex.color, [1.0; 4]);
        }
        fs::write(&path, &encoded[..encoded.len() - 1]).unwrap();
        assert!(
            reader
                .read_into(11, &mut vertices, &mut whitewater)
                .is_err()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_v6_decodes_colored_vertices_and_defaults_new_settings() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-v6-current-{}",
            std::process::id()
        ));
        let directory = root.join("frames");
        let settings = FluidSettings::default();
        fs::create_dir_all(&directory).unwrap();

        let mut manifest = Vec::new();
        manifest.extend_from_slice(MAGIC);
        write_u32(&mut manifest, MESH_VERTEX_FORMAT_VERSION).unwrap();
        manifest.extend_from_slice(manifold_fluids::UPSTREAM_REVISION.as_bytes());
        write_f64(&mut manifest, TICK).unwrap();
        write_settings(&mut manifest, settings).unwrap();
        // The v7 writer appends the optional domain and six boundary flags.
        manifest.truncate(manifest.len() - 7);
        fs::write(directory.join(MANIFEST), manifest).unwrap();

        let (vertices, whitewater, obstacle, stats) = frame();
        let mut payload = Vec::new();
        write_u64(&mut payload, 13).unwrap();
        write_len(&mut payload, vertices.len(), MAX_VERTICES).unwrap();
        write_float_records(&mut payload, &vertices).unwrap();
        write_instances(&mut payload, &whitewater.foam).unwrap();
        write_instances(&mut payload, &whitewater.bubbles).unwrap();
        write_instances(&mut payload, &whitewater.spray).unwrap();
        write_transform(&mut payload, obstacle).unwrap();
        write_u32(&mut payload, stats.particles).unwrap();
        write_u32(&mut payload, stats.triangles).unwrap();
        write_u32(&mut payload, stats.substeps).unwrap();
        write_f64(&mut payload, stats.simulation_ms).unwrap();
        write_f64(&mut payload, stats.meshing_ms).unwrap();
        fs::write(
            frame_path(&directory, 13),
            zstd::stream::encode_all(payload.as_slice(), 1).unwrap(),
        )
        .unwrap();

        let reader = CacheReader::open(Arc::new(directory.clone()), settings).unwrap();
        let mut decoded_vertices = Vec::new();
        let mut decoded_whitewater = WhitewaterFrame::default();
        reader
            .read_into(13, &mut decoded_vertices, &mut decoded_whitewater)
            .unwrap();
        assert_eq!(decoded_vertices[0].color, vertices[0].color);

        let mut changed = settings;
        changed.domain = Some(Transform {
            scale: [4.0, 3.0, 5.0],
            ..Transform::default()
        });
        assert!(CacheReader::open(Arc::new(directory.clone()), changed).is_err());
        changed = settings;
        changed.boundary_collisions[0] = false;
        assert!(CacheReader::open(Arc::new(directory), changed).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_v7_decodes_unpaired_frames_after_v8_bump() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-v7-legacy-{}",
            std::process::id()
        ));
        let directory = root.join("frames");
        let settings = FluidSettings::default();
        fs::create_dir_all(&directory).unwrap();
        let mut manifest = Vec::new();
        manifest.extend_from_slice(MAGIC);
        write_u32(&mut manifest, DOMAIN_FORMAT_VERSION).unwrap();
        manifest.extend_from_slice(manifold_fluids::UPSTREAM_REVISION.as_bytes());
        write_f64(&mut manifest, TICK).unwrap();
        write_settings(&mut manifest, settings).unwrap();
        fs::write(directory.join(MANIFEST), manifest).unwrap();
        let mut payload = Vec::new();
        write_unpaired_payload(&mut payload, 17);
        fs::write(
            frame_path(&directory, 17),
            zstd::stream::encode_all(payload.as_slice(), 1).unwrap(),
        )
        .unwrap();

        let reader = CacheReader::open(Arc::new(directory.clone()), settings).unwrap();
        let mut vertices = Vec::new();
        let mut whitewater = WhitewaterFrame::default();
        reader
            .read_into(17, &mut vertices, &mut whitewater)
            .unwrap();
        let (_, _, paired) = reader
            .read_paired_into(17, &mut vertices, &mut whitewater, None)
            .unwrap();
        assert!(!paired);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_v8_preserves_paired_frames_and_rejects_nondefault_seed() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-v8-legacy-{}",
            std::process::id()
        ));
        let directory = Arc::new(root.join("frames"));
        fs::create_dir_all(directory.as_ref()).unwrap();
        let settings = FluidSettings::default();
        let mut manifest = Vec::new();
        manifest.extend_from_slice(MAGIC);
        write_u32(&mut manifest, PAIRED_FORMAT_VERSION).unwrap();
        manifest.extend_from_slice(manifold_fluids::UPSTREAM_REVISION.as_bytes());
        write_f64(&mut manifest, TICK).unwrap();
        write_settings(&mut manifest, settings).unwrap();
        fs::write(directory.join(MANIFEST), manifest).unwrap();
        let (vertices, whitewater, obstacle, stats) = frame();
        let mut rigid = CoupledRigidFrame {
            stamp: manifold_physics::TickStamp { epoch: 9, tick: 19 },
            ..Default::default()
        };
        rigid.poses[0].pos = [2.0, 3.0, 4.0];
        let mut payload = Vec::new();
        write_frame(
            &mut payload,
            19,
            &vertices,
            &whitewater,
            obstacle,
            stats,
            Some(&rigid),
        )
        .unwrap();
        fs::write(
            frame_path(&directory, 19),
            zstd::stream::encode_all(payload.as_slice(), 1).unwrap(),
        )
        .unwrap();
        let reader = CacheReader::open(Arc::clone(&directory), settings).unwrap();
        let mut decoded_rigid = CoupledRigidFrame::default();
        let (_, _, paired) = reader
            .read_paired_into(
                19,
                &mut Vec::new(),
                &mut WhitewaterFrame::default(),
                Some(&mut decoded_rigid),
            )
            .unwrap();
        assert!(paired);
        assert_eq!(decoded_rigid.stamp, rigid.stamp);
        assert_eq!(decoded_rigid.poses[0].pos, rigid.poses[0].pos);
        let changed = FluidSettings {
            seed: 1,
            ..settings
        };
        assert!(CacheReader::open(directory, changed).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_rejects_changed_solver_sources() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-source-identity-{}",
            std::process::id()
        ));
        let directory = Arc::new(root.join("frames"));
        let settings = FluidSettings::default();
        let _writer = CacheWriter::create(Arc::clone(&directory), settings).unwrap();
        let path = directory.join(MANIFEST);
        let mut bytes = fs::read(&path).unwrap();
        let identity_offset = MAGIC.len() + 4 + manifold_fluids::UPSTREAM_REVISION.len() + 8;
        bytes[identity_offset] ^= 1;
        fs::write(path, bytes).unwrap();
        assert!(
            CacheReader::open(directory, settings)
                .err()
                .unwrap()
                .contains("sources do not match")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_paired_round_trip_rejects_unpaired_reader_and_restores_stamp() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-paired-{}",
            std::process::id()
        ));
        let directory = Arc::new(root.join("frames"));
        let settings = FluidSettings::default();
        let writer = CacheWriter::create(directory.clone(), settings).unwrap();
        let (vertices, whitewater, obstacle, stats) = frame();
        let mut rigid = CoupledRigidFrame {
            stamp: manifold_physics::TickStamp { epoch: 9, tick: 19 },
            ..Default::default()
        };
        rigid.poses[0].pos = [2.0, 3.0, 4.0];
        rigid.copies.push(Transform {
            pos: [5.0, 6.0, 7.0],
            ..Transform::default()
        });
        writer
            .append_paired(19, &vertices, &whitewater, obstacle, stats, Some(&rigid))
            .unwrap();
        let reader = CacheReader::open(directory, settings).unwrap();
        let mut decoded_vertices = Vec::new();
        let mut decoded_whitewater = WhitewaterFrame::default();
        assert!(
            reader
                .read_into(19, &mut decoded_vertices, &mut decoded_whitewater)
                .is_err()
        );
        let mut decoded_rigid = CoupledRigidFrame::default();
        let (decoded_obstacle, decoded_stats, paired) = reader
            .read_paired_into(
                19,
                &mut decoded_vertices,
                &mut decoded_whitewater,
                Some(&mut decoded_rigid),
            )
            .unwrap();
        assert!(paired);
        assert_eq!(decoded_obstacle, obstacle);
        assert_eq!(decoded_stats, stats);
        assert_eq!(decoded_rigid.stamp, rigid.stamp);
        assert_eq!(decoded_rigid.poses[0].pos, rigid.poses[0].pos);
        assert_eq!(decoded_rigid.copies, rigid.copies);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_paired_rejects_mismatched_tick_count_and_nonfinite_pose() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-paired-invalid-{}",
            std::process::id()
        ));
        let directory = Arc::new(root.join("frames"));
        let settings = FluidSettings::default();
        let writer = CacheWriter::create(directory.clone(), settings).unwrap();
        let (vertices, whitewater, obstacle, stats) = frame();
        let mut mismatched = CoupledRigidFrame::default();
        mismatched.stamp.tick = 3;
        assert!(
            writer
                .append_paired(
                    4,
                    &vertices,
                    &whitewater,
                    obstacle,
                    stats,
                    Some(&mismatched)
                )
                .is_err()
        );
        let reader = CacheReader::open(directory.clone(), settings).unwrap();
        let path = frame_path(directory.as_ref(), 4);
        let mut payload = Vec::new();
        write_paired_payload(&mut payload, 4, MAX_COPIES as u32 + 1, false);
        fs::write(
            &path,
            zstd::stream::encode_all(payload.as_slice(), 1).unwrap(),
        )
        .unwrap();
        let mut decoded_vertices = Vec::new();
        let mut decoded_whitewater = WhitewaterFrame::default();
        let mut decoded_rigid = CoupledRigidFrame::default();
        assert!(
            reader
                .read_paired_into(
                    4,
                    &mut decoded_vertices,
                    &mut decoded_whitewater,
                    Some(&mut decoded_rigid),
                )
                .is_err()
        );
        payload.clear();
        write_paired_payload(&mut payload, 4, 0, true);
        fs::write(
            &path,
            zstd::stream::encode_all(payload.as_slice(), 1).unwrap(),
        )
        .unwrap();
        assert!(
            reader
                .read_paired_into(
                    4,
                    &mut decoded_vertices,
                    &mut decoded_whitewater,
                    Some(&mut decoded_rigid),
                )
                .is_err()
        );
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

        let mut changed_domain = settings;
        changed_domain.domain = Some(Transform {
            scale: [4.0, 3.0, 5.0],
            ..Transform::default()
        });
        assert!(CacheReader::open(Arc::new(directory.clone()), changed_domain).is_err());
        let mut changed_boundary = settings;
        changed_boundary.boundary_collisions[0] = false;
        assert!(CacheReader::open(Arc::new(directory.clone()), changed_boundary).is_err());

        let mut changed = settings;
        changed.liquid.viscosity = 0.25;
        assert!(CacheReader::open(Arc::new(directory.clone()), changed).is_err());
        changed.liquid = manifold_fluids::LiquidOptions {
            viscosity: 0.0,
            surface_tension: 0.1,
        };
        assert!(CacheReader::open(Arc::new(directory.clone()), changed).is_err());
        changed.liquid = manifold_fluids::LiquidOptions::default();
        changed.time_steps.cfl = 2;
        assert!(CacheReader::open(Arc::new(directory), changed).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cache_v4_preserves_liquid_and_defaults_time_steps() {
        let root = std::env::temp_dir().join(format!(
            "manifold-fluid-cache-v4-time-steps-{}",
            std::process::id()
        ));
        let directory = root.join("frames");
        let settings = FluidSettings {
            liquid: manifold_fluids::LiquidOptions {
                viscosity: 0.25,
                surface_tension: 0.1,
            },
            ..FluidSettings::default()
        };
        write_v4_liquid_manifest(&directory);
        assert!(CacheReader::open(Arc::new(directory.clone()), settings).is_ok());

        let mut changed_domain = settings;
        changed_domain.domain = Some(Transform {
            scale: [4.0, 3.0, 5.0],
            ..Transform::default()
        });
        assert!(CacheReader::open(Arc::new(directory.clone()), changed_domain).is_err());
        let mut changed_boundary = settings;
        changed_boundary.boundary_collisions[0] = false;
        assert!(CacheReader::open(Arc::new(directory.clone()), changed_boundary).is_err());

        let mut changed = settings;
        changed.time_steps.min_substeps = 2;
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
