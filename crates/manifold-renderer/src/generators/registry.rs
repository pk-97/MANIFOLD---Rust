use manifold_core::GeneratorType;
use crate::generator::Generator;
use super::basic_shapes_snap::BasicShapesSnapGenerator;
use super::concentric_tunnel::ConcentricTunnelGenerator;
use super::duocylinder::DuocylinderGenerator;
use super::flowfield::FlowfieldGenerator;
use super::fractal_zoom::FractalZoomGenerator;
use super::lissajous::LissajousGenerator;
use super::number_station::NumberStationGenerator;
use super::oscilloscope_xy::OscilloscopeXYGenerator;
use super::plasma::PlasmaGenerator;
use super::reaction_diffusion::ReactionDiffusionGenerator;
use super::strange_attractor::StrangeAttractorGenerator;
use super::tesseract::TesseractGenerator;
use super::wireframe_zoo::WireframeZooGenerator;

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
            GeneratorType::Tesseract => Some(Box::new(TesseractGenerator::new(device, self.target_format))),
            GeneratorType::Duocylinder => Some(Box::new(DuocylinderGenerator::new(device, self.target_format))),
            GeneratorType::Lissajous => Some(Box::new(LissajousGenerator::new(device, self.target_format))),
            GeneratorType::WireframeZoo => Some(Box::new(WireframeZooGenerator::new(device, self.target_format))),
            GeneratorType::OscilloscopeXY => Some(Box::new(OscilloscopeXYGenerator::new(device, self.target_format))),
            GeneratorType::ReactionDiffusion => Some(Box::new(ReactionDiffusionGenerator::new(device, self.target_format))),
            GeneratorType::Flowfield => Some(Box::new(FlowfieldGenerator::new(device, self.target_format))),
            GeneratorType::StrangeAttractor => Some(Box::new(StrangeAttractorGenerator::new(device, self.target_format))),
            _ => {
                log::warn!("Generator type {:?} not yet implemented", gen_type);
                None
            }
        }
    }
}
