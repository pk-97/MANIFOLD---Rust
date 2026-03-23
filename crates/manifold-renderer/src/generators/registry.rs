use manifold_core::GeneratorTypeId;
use crate::generator::Generator;
use super::basic_shapes_snap::BasicShapesSnapGenerator;
use super::compute_strange_attractor::ComputeStrangeAttractorGenerator;
use super::concentric_tunnel::ConcentricTunnelGenerator;
use super::duocylinder::DuocylinderGenerator;
use super::flowfield::FlowfieldGenerator;
use super::fluid_simulation::FluidSimulationGenerator;
use super::fluid_simulation_3d::FluidSimulation3DGenerator;
use super::fractal_zoom::FractalZoomGenerator;
use super::lissajous::LissajousGenerator;
use super::mycelium::MyceliumGenerator;
use super::number_station::NumberStationGenerator;
use super::oscilloscope_xy::OscilloscopeXYGenerator;
use super::parametric_surface::ParametricSurfaceGenerator;
use super::plasma::PlasmaGenerator;
use super::reaction_diffusion::ReactionDiffusionGenerator;
use super::strange_attractor::StrangeAttractorGenerator;
use super::tesseract::TesseractGenerator;
use super::mri_volume::MriVolumeGenerator;
use super::wireframe_zoo::WireframeZooGenerator;

/// Factory that maps GeneratorTypeId to concrete Generator instances.
/// Pipeline compilation happens at creation time (expensive — do at startup or first use).
pub struct GeneratorRegistry {
    target_format: wgpu::TextureFormat,
}

impl GeneratorRegistry {
    pub fn new(target_format: wgpu::TextureFormat) -> Self {
        Self { target_format }
    }

    /// Create a new generator instance for the given type.
    pub fn create(&self, device: &wgpu::Device, gen_type: &GeneratorTypeId) -> Option<Box<dyn Generator>> {
        let fmt = self.target_format;
        if *gen_type == GeneratorTypeId::PLASMA {
            Some(Box::new(PlasmaGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::FRACTAL_ZOOM {
            Some(Box::new(FractalZoomGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::BASIC_SHAPES_SNAP {
            Some(Box::new(BasicShapesSnapGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::CONCENTRIC_TUNNEL {
            Some(Box::new(ConcentricTunnelGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::NUMBER_STATION {
            Some(Box::new(NumberStationGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::TESSERACT {
            Some(Box::new(TesseractGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::DUOCYLINDER {
            Some(Box::new(DuocylinderGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::LISSAJOUS {
            Some(Box::new(LissajousGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::WIREFRAME_ZOO {
            Some(Box::new(WireframeZooGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::OSCILLOSCOPE_XY {
            Some(Box::new(OscilloscopeXYGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::REACTION_DIFFUSION {
            Some(Box::new(ReactionDiffusionGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::FLOWFIELD {
            Some(Box::new(FlowfieldGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::STRANGE_ATTRACTOR {
            Some(Box::new(StrangeAttractorGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::PARAMETRIC_SURFACE {
            Some(Box::new(ParametricSurfaceGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::MYCELIUM {
            Some(Box::new(MyceliumGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::COMPUTE_STRANGE_ATTRACTOR {
            Some(Box::new(ComputeStrangeAttractorGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::FLUID_SIMULATION {
            Some(Box::new(FluidSimulationGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::FLUID_SIMULATION_3D {
            Some(Box::new(FluidSimulation3DGenerator::new(device, fmt)))
        } else if *gen_type == GeneratorTypeId::MRI_VOLUME {
            Some(Box::new(MriVolumeGenerator::new(device, fmt)))
        } else {
            log::warn!("Generator type {:?} not yet implemented", gen_type);
            None
        }
    }
}
