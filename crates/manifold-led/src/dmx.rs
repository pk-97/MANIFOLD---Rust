//! ArtNet/DMX byte-packing logic.
//! Pure functions — no GPU, no networking.
//! Unity equivalent: ArtNetDmxConverter.cs

use crate::types::*;

/// Write a standard ArtNet DMX packet header (18 bytes).
/// Unity: ArtNetDmxConverter.WriteArtNetHeader()
pub fn write_artnet_header(packet: &mut [u8], universe: u16) {
    // "Art-Net\0"
    packet[0] = b'A';
    packet[1] = b'r';
    packet[2] = b't';
    packet[3] = b'-';
    packet[4] = b'N';
    packet[5] = b'e';
    packet[6] = b't';
    packet[7] = 0;

    // OpCode: ArtDmx (0x5000) little-endian
    packet[8] = 0x00;
    packet[9] = 0x50;

    // Protocol version: 14 (big-endian)
    packet[10] = 0x00;
    packet[11] = 14;

    // Sequence (0 = disabled)
    packet[12] = 0;

    // Physical port
    packet[13] = 0;

    // Universe (little-endian)
    packet[14] = (universe & 0xFF) as u8;
    packet[15] = ((universe >> 8) & 0xFF) as u8;

    // Length (big-endian): 512
    packet[16] = ((DMX_UNIVERSE_SIZE >> 8) & 0xFF) as u8;
    packet[17] = (DMX_UNIVERSE_SIZE & 0xFF) as u8;
}

/// Write a pixel's RGB data to the correct universe buffer(s).
/// `global_channel` is 0-based across the entire linear DMX address space.
/// Handles pixel data straddling a universe boundary.
/// Unity: ArtNetDmxConverter.WritePixelToUniverses()
pub fn write_pixel_to_universes(
    dmx_buffers: &mut [Vec<u8>],
    global_channel: usize,
    r: u8,
    g: u8,
    b: u8,
    is_bgr: bool,
) {
    let universe_idx = global_channel / DMX_UNIVERSE_SIZE;
    let local_ch = global_channel % DMX_UNIVERSE_SIZE;

    if universe_idx >= dmx_buffers.len() {
        return;
    }

    let c0 = if is_bgr { b } else { r };
    let c1 = g;
    let c2 = if is_bgr { r } else { b };

    // Fast path: all 3 bytes fit in same universe
    if local_ch + 2 < DMX_UNIVERSE_SIZE {
        dmx_buffers[universe_idx][local_ch] = c0;
        dmx_buffers[universe_idx][local_ch + 1] = c1;
        dmx_buffers[universe_idx][local_ch + 2] = c2;
        return;
    }

    // Slow path: pixel straddles universe boundary
    let channels = [c0, c1, c2];
    for (i, &ch_val) in channels.iter().enumerate() {
        let g_ch = global_channel + i;
        let u_idx = g_ch / DMX_UNIVERSE_SIZE;
        let l_ch = g_ch % DMX_UNIVERSE_SIZE;
        if u_idx < dmx_buffers.len() {
            dmx_buffers[u_idx][l_ch] = ch_val;
        }
    }
}

/// Sample a strip column from the pixel grid and write to universe-based DMX buffers.
/// Resolume-style linear addressing with auto-wrapping across 512-channel universes.
///
/// `pixels` is tightly-packed RGBA8 data, laid out as `[row * pixel_width + col]`.
/// `pixel_width` is the number of columns (strips).
/// `global_start_channel` is 0-based.
///
/// Unity: ArtNetDmxConverter.SampleStripToUniverses()
pub fn sample_strip_to_universes(
    dmx_buffers: &mut [Vec<u8>],
    pixels: &[u8],
    pixel_width: usize,
    strip_index: usize,
    leds_per_strip: usize,
    global_start_channel: usize,
    is_bgr: bool,
    brightness: f32,
) {
    let mut ch = global_start_channel;
    let bright_int = (brightness.clamp(0.0, 1.0) * 255.0).round() as u32;
    let need_scale = bright_int < 255;

    for led in 0..leds_per_strip {
        let pixel_offset = (led * pixel_width + strip_index) * 4; // RGBA8
        if pixel_offset + 2 >= pixels.len() {
            break;
        }

        let mut r = pixels[pixel_offset];
        let mut g = pixels[pixel_offset + 1];
        let mut b = pixels[pixel_offset + 2];

        if need_scale {
            r = ((r as u32 * bright_int) >> 8) as u8;
            g = ((g as u32 * bright_int) >> 8) as u8;
            b = ((b as u32 * bright_int) >> 8) as u8;
        }

        write_pixel_to_universes(dmx_buffers, ch, r, g, b, is_bgr);
        ch += CHANNELS_PER_LED;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_artnet_header() {
        let mut packet = vec![0u8; ARTNET_HEADER_SIZE + DMX_UNIVERSE_SIZE];
        write_artnet_header(&mut packet, 3);

        assert_eq!(&packet[0..8], b"Art-Net\0");
        assert_eq!(packet[8], 0x00); // OpCode lo
        assert_eq!(packet[9], 0x50); // OpCode hi
        assert_eq!(packet[10], 0x00); // Version hi
        assert_eq!(packet[11], 14); // Version lo
        assert_eq!(packet[14], 3); // Universe lo
        assert_eq!(packet[15], 0); // Universe hi
        assert_eq!(packet[16], 0x02); // Length hi (512)
        assert_eq!(packet[17], 0x00); // Length lo
    }

    #[test]
    fn test_write_pixel_fast_path() {
        let mut buffers = vec![vec![0u8; DMX_UNIVERSE_SIZE]];
        write_pixel_to_universes(&mut buffers, 0, 255, 128, 64, false);
        assert_eq!(buffers[0][0], 255); // R
        assert_eq!(buffers[0][1], 128); // G
        assert_eq!(buffers[0][2], 64); // B
    }

    #[test]
    fn test_write_pixel_bgr() {
        let mut buffers = vec![vec![0u8; DMX_UNIVERSE_SIZE]];
        write_pixel_to_universes(&mut buffers, 0, 255, 128, 64, true);
        assert_eq!(buffers[0][0], 64); // B
        assert_eq!(buffers[0][1], 128); // G
        assert_eq!(buffers[0][2], 255); // R
    }

    #[test]
    fn test_write_pixel_universe_boundary() {
        let mut buffers = vec![
            vec![0u8; DMX_UNIVERSE_SIZE],
            vec![0u8; DMX_UNIVERSE_SIZE],
        ];
        // Start at channel 511 — straddles into universe 1
        write_pixel_to_universes(&mut buffers, 511, 10, 20, 30, false);
        assert_eq!(buffers[0][511], 10); // R in universe 0
        assert_eq!(buffers[1][0], 20); // G in universe 1
        assert_eq!(buffers[1][1], 30); // B in universe 1
    }

    #[test]
    fn test_sample_strip_brightness_scaling() {
        let mut buffers = vec![vec![0u8; DMX_UNIVERSE_SIZE]];
        // 1 strip, 1 LED, RGBA pixel
        let pixels = [200u8, 100, 50, 255];
        sample_strip_to_universes(
            &mut buffers, &pixels, 1, 0, 1, 0, false, 0.5,
        );
        // brightness 0.5 → bright_int = 128
        // 200 * 128 >> 8 = 100
        // 100 * 128 >> 8 = 50
        // 50 * 128 >> 8 = 25
        assert_eq!(buffers[0][0], 100);
        assert_eq!(buffers[0][1], 50);
        assert_eq!(buffers[0][2], 25);
    }
}
