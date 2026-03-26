use manifold_core::GeneratorTypeId;
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
    target_format: wgpu::TextureFormat,
}

impl GeneratorRegistry {
    pub fn new(target_format: wgpu::TextureFormat) -> Self {
        Self { target_format }
    }

    /// Create a new generator instance for the given type.
    pub fn create(
        &self,
        device: &wgpu::Device,
        gen_type: &GeneratorTypeId,
        hal_ctx: Option<&crate::hal_context::HalContext>,
        #[cfg(target_os = "macos")] native_device: Option<&manifold_gpu::GpuDevice>,
    ) -> Option<Box<dyn Generator>> {
        let fmt = self.target_format;
        let _ = &hal_ctx; // suppress unused warning when hal-encoding off
        if *gen_type == GeneratorTypeId::PLASMA {
            Some(Box::new(PlasmaGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")]
                native_device,
            )))
        } else if *gen_type == GeneratorTypeId::BASIC_SHAPES_SNAP {
            Some(Box::new(BasicShapesSnapGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else if *gen_type == GeneratorTypeId::CONCENTRIC_TUNNEL {
            Some(Box::new(ConcentricTunnelGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else if *gen_type == GeneratorTypeId::TESSERACT {
            Some(Box::new(TesseractGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else if *gen_type == GeneratorTypeId::DUOCYLINDER {
            Some(Box::new(DuocylinderGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else if *gen_type == GeneratorTypeId::LISSAJOUS {
            Some(Box::new(LissajousGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else if *gen_type == GeneratorTypeId::WIREFRAME_ZOO {
            Some(Box::new(WireframeZooGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else if *gen_type == GeneratorTypeId::OSCILLOSCOPE_XY {
            Some(Box::new(OscilloscopeXYGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else if *gen_type == GeneratorTypeId::PARAMETRIC_SURFACE {
            Some(Box::new(ParametricSurfaceGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else if *gen_type == GeneratorTypeId::MYCELIUM {
            Some(Box::new(MyceliumGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else if *gen_type == GeneratorTypeId::FLUID_SIMULATION {
            Some(Box::new(FluidSimulationGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else if *gen_type == GeneratorTypeId::FLUID_SIMULATION_3D {
            Some(Box::new(FluidSimulation3DGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else if *gen_type == GeneratorTypeId::MRI_VOLUME {
            Some(Box::new(MriVolumeGenerator::new(
                device, fmt, hal_ctx,
                #[cfg(target_os = "macos")] native_device,
            )))
        } else {
            log::warn!("Generator type {:?} not yet implemented", gen_type);
            None
        }
    }
}
