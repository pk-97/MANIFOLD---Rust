use super::basic_shapes_snap::BasicShapesSnapGenerator;
use super::black_hole::BlackHoleGenerator;
use super::concentric_tunnel::ConcentricTunnelGenerator;
use super::duocylinder::DuocylinderGenerator;
use super::fluid_simulation::FluidSimulationGenerator;
use super::fluid_simulation_3d::FluidSimulation3DGenerator;
use super::galactic_rock::GalacticRockGenerator;
use super::metallic_glass::MetallicGlassGenerator;
use super::lissajous::LissajousGenerator;
use super::mri_volume::MriVolumeGenerator;
use super::mycelium::MyceliumGenerator;
use super::oily_fluid::OilyFluidGenerator;
use super::oscilloscope_xy::OscilloscopeXYGenerator;
use super::parametric_surface::ParametricSurfaceGenerator;
use super::plasma::PlasmaGenerator;
use super::strange_attractor::StrangeAttractorGenerator;
use super::tesseract::TesseractGenerator;
use super::wireframe_zoo::WireframeZooGenerator;
use crate::generator::Generator;
use manifold_core::GeneratorTypeId;
use manifold_gpu::{GpuDevice, GpuTextureFormat};

/// Factory that maps GeneratorTypeId to concrete Generator instances.
/// Pipeline compilation happens at creation time (expensive — do at startup or first use).
pub struct GeneratorRegistry {
    target_format: GpuTextureFormat,
}

impl GeneratorRegistry {
    pub fn new(target_format: GpuTextureFormat) -> Self {
        Self { target_format }
    }

    /// Pre-compile all generator pipelines into the binary archive.
    /// Creates and immediately drops each generator — the compiled Metal pipeline
    /// binaries persist in the archive. Call at startup before `save_pipeline_archive()`.
    pub fn prewarm_all(&self, device: &GpuDevice) {
        let all_types = [
            GeneratorTypeId::PLASMA,
            GeneratorTypeId::BASIC_SHAPES_SNAP,
            GeneratorTypeId::CONCENTRIC_TUNNEL,
            GeneratorTypeId::TESSERACT,
            GeneratorTypeId::DUOCYLINDER,
            GeneratorTypeId::LISSAJOUS,
            GeneratorTypeId::WIREFRAME_ZOO,
            GeneratorTypeId::OSCILLOSCOPE_XY,
            GeneratorTypeId::PARAMETRIC_SURFACE,
            GeneratorTypeId::MYCELIUM,
            GeneratorTypeId::FLUID_SIMULATION,
            GeneratorTypeId::FLUID_SIMULATION_3D,
            GeneratorTypeId::MRI_VOLUME,
            GeneratorTypeId::BLACK_HOLE,
            GeneratorTypeId::GALACTIC_ROCK,
            GeneratorTypeId::METALLIC_GLASS,
            GeneratorTypeId::COMPUTE_STRANGE_ATTRACTOR,
            GeneratorTypeId::OILY_FLUID,
        ];
        log::info!("Pre-warming {} generator pipelines...", all_types.len());
        for gen_type in &all_types {
            let _ = self.create(device, gen_type);
        }
        log::info!("Generator pipeline pre-warm complete");
    }

    /// Create a new generator instance for the given type.
    pub fn create(
        &self,
        device: &GpuDevice,
        gen_type: &GeneratorTypeId,
    ) -> Option<Box<dyn Generator>> {
        let _fmt = self.target_format;
        if *gen_type == GeneratorTypeId::PLASMA {
            Some(Box::new(PlasmaGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::BASIC_SHAPES_SNAP {
            Some(Box::new(BasicShapesSnapGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::CONCENTRIC_TUNNEL {
            Some(Box::new(ConcentricTunnelGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::TESSERACT {
            Some(Box::new(TesseractGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::DUOCYLINDER {
            Some(Box::new(DuocylinderGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::LISSAJOUS {
            Some(Box::new(LissajousGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::WIREFRAME_ZOO {
            Some(Box::new(WireframeZooGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::OSCILLOSCOPE_XY {
            Some(Box::new(OscilloscopeXYGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::PARAMETRIC_SURFACE {
            Some(Box::new(ParametricSurfaceGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::MYCELIUM {
            Some(Box::new(MyceliumGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::FLUID_SIMULATION {
            Some(Box::new(FluidSimulationGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::FLUID_SIMULATION_3D {
            Some(Box::new(FluidSimulation3DGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::MRI_VOLUME {
            Some(Box::new(MriVolumeGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::BLACK_HOLE {
            Some(Box::new(BlackHoleGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::GALACTIC_ROCK {
            Some(Box::new(GalacticRockGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::METALLIC_GLASS {
            Some(Box::new(MetallicGlassGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::COMPUTE_STRANGE_ATTRACTOR {
            Some(Box::new(StrangeAttractorGenerator::new(device)))
        } else if *gen_type == GeneratorTypeId::OILY_FLUID {
            Some(Box::new(OilyFluidGenerator::new(device)))
        } else {
            log::warn!("Generator type {:?} not yet implemented", gen_type);
            None
        }
    }
}
