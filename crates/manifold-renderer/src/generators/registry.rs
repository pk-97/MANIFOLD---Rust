use manifold_core::GeneratorTypeId;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use crate::generator::Generator;
use super::basic_shapes_snap::BasicShapesSnapGenerator;
use super::concentric_tunnel::ConcentricTunnelGenerator;
use super::duocylinder::DuocylinderGenerator;
use super::fluid_simulation::FluidSimulationGenerator;
use super::fluid_simulation_3d::FluidSimulation3DGenerator;
use super::lissajous::LissajousGenerator;
use super::mycelium::MyceliumGenerator;
use super::oscilloscope_xy::OscilloscopeXYGenerator;
use super::parametric_surface::ParametricSurfaceGenerator;
use super::plasma::PlasmaGenerator;
use super::tesseract::TesseractGenerator;
use super::mri_volume::MriVolumeGenerator;
use super::wireframe_zoo::WireframeZooGenerator;

/// Factory that maps GeneratorTypeId to concrete Generator instances.
/// Pipeline compilation happens at creation time (expensive — do at startup or first use).
pub struct GeneratorRegistry {
    target_format: GpuTextureFormat,
}

impl GeneratorRegistry {
    pub fn new(target_format: GpuTextureFormat) -> Self {
        Self { target_format }
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
        } else {
            log::warn!("Generator type {:?} not yet implemented", gen_type);
            None
        }
    }
}
