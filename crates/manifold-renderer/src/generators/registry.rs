use manifold_core::GeneratorType;
use crate::generator::Generator;
use super::basic_shapes_snap::BasicShapesSnapGenerator;
use super::concentric_tunnel::ConcentricTunnelGenerator;
use super::fractal_zoom::FractalZoomGenerator;
use super::number_station::NumberStationGenerator;
use super::plasma::PlasmaGenerator;

/// Factory that maps GeneratorType to concrete Generator instances.
/// Pipeline compilation happens at creation time (expensive — do at startup or first use).
pub struct GeneratorRegistry {
    target_format: wgpu::TextureFormat,
}

impl GeneratorRegistry {
    pub fn new(target_format: wgpu::TextureFormat) -> Self {
        Self { target_format }
    }

    /// Create a new generator instance for the given type.
    pub fn create(&self, device: &wgpu::Device, gen_type: GeneratorType) -> Option<Box<dyn Generator>> {
        match gen_type {
            GeneratorType::Plasma => Some(Box::new(PlasmaGenerator::new(device, self.target_format))),
            GeneratorType::FractalZoom => Some(Box::new(FractalZoomGenerator::new(device, self.target_format))),
            GeneratorType::BasicShapesSnap => Some(Box::new(BasicShapesSnapGenerator::new(device, self.target_format))),
            GeneratorType::ConcentricTunnel => Some(Box::new(ConcentricTunnelGenerator::new(device, self.target_format))),
            GeneratorType::NumberStation => Some(Box::new(NumberStationGenerator::new(device, self.target_format))),
            _ => {
                log::warn!("Generator type {:?} not yet implemented", gen_type);
                None
            }
        }
    }
}
