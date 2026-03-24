use serde::{Deserialize, Serialize};

// ─── Constants (exact match: LEDConstants.cs) ───

/// Default number of LED strips.
pub const DEFAULT_STRIP_COUNT: u32 = 8;
/// Default LEDs per strip.
pub const DEFAULT_LEDS_PER_STRIP: u32 = 120;
/// Default ArtNet target IP.
pub const DEFAULT_ARTNET_IP: &str = "192.168.2.18";
/// Default ArtNet port (standard).
pub const DEFAULT_ARTNET_PORT: u16 = 6454;
/// RGB channels per LED pixel.
pub const CHANNELS_PER_LED: usize = 3;
/// DMX universe size in channels.
pub const DMX_UNIVERSE_SIZE: usize = 512;
/// ArtNet packet header size in bytes.
pub const ARTNET_HEADER_SIZE: usize = 18;
/// Physical installation: strips per side.
pub const STRIPS_PER_SIDE: u32 = 4;
/// ArtNet OpPoll opcode.
pub const ARTNET_OP_POLL: u16 = 0x2000;
/// ArtNet protocol version.
pub const ARTNET_PROTOCOL_VERSION: u16 = 14;

// ─── Enums (exact match: ExternalOutputType.cs) ───

/// External output backend type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExternalOutputType {
    #[default]
    ArtNet,
}

/// How LED strips are addressed across DMX universes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum StripAddressing {
    /// Each strip starts at the beginning of its own DMX universe.
    #[default]
    PerUniverse,
    /// Strips pack contiguously, auto-wrapping across 512-channel universes.
    Packed,
}

// ─── Settings (exact match: LedSettings.cs, minus energy gate) ───

/// Configuration for LED / external output.
/// Unity equivalent: LedSettings ScriptableObject.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub output_type: ExternalOutputType,

    // ArtNet
    #[serde(default = "default_left_edge_width")]
    pub left_edge_width: f32,
    #[serde(default = "default_right_edge_width")]
    pub right_edge_width: f32,
    #[serde(default = "default_artnet_ip")]
    pub artnet_ip: String,
    #[serde(default = "default_artnet_port")]
    pub artnet_port: u16,
    #[serde(default)]
    pub start_universe: u16,
    #[serde(default = "default_strip_count")]
    pub strip_count: u32,
    #[serde(default = "default_leds_per_strip")]
    pub leds_per_strip: u32,
    #[serde(default = "default_true")]
    pub is_bgr: bool,
    #[serde(default)]
    pub strip_addressing: StripAddressing,

    /// Spatial blur radius in source texels. Smooths out single-pixel flicker
    /// on physical LEDs. 0 = no blur, 8-16 = good default for 1080p source.
    #[serde(default = "default_blur_radius")]
    pub blur_radius: f32,
}

impl Default for LedSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            output_type: ExternalOutputType::ArtNet,
            left_edge_width: 0.2,
            right_edge_width: 0.2,
            artnet_ip: DEFAULT_ARTNET_IP.to_string(),
            artnet_port: DEFAULT_ARTNET_PORT,
            start_universe: 0,
            strip_count: DEFAULT_STRIP_COUNT,
            leds_per_strip: DEFAULT_LEDS_PER_STRIP,
            is_bgr: true,
            strip_addressing: StripAddressing::PerUniverse,
            blur_radius: 12.0,
        }
    }
}

fn default_true() -> bool { true }
fn default_left_edge_width() -> f32 { 0.2 }
fn default_right_edge_width() -> f32 { 0.2 }
fn default_artnet_ip() -> String { DEFAULT_ARTNET_IP.to_string() }
fn default_artnet_port() -> u16 { DEFAULT_ARTNET_PORT }
fn default_strip_count() -> u32 { DEFAULT_STRIP_COUNT }
fn default_leds_per_strip() -> u32 { DEFAULT_LEDS_PER_STRIP }
fn default_blur_radius() -> f32 { 12.0 }
