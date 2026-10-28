//! GPU proofs for the wet-surface look: the blend of albedo and roughness
//! against the closed form of Lekner and Dorf, read from a wet map; the
//! drying by the age of the last touch, the touched heights, an edge read
//! from the touched texels only, and that a point off the map, under an unplaced map or in a scene
//! without one is left alone.
use super::water_march_gpu_tests::shared_buffer;
use super::wetness::{WetUniforms, wet_uniforms};
use super::{FieldObject, model_matrix};
use manifold_gpu::*;
use manifold_node_engine::scene::wet_map::{
    HEADER_WORDS, LAYOUT, TEXEL_WORDS, height_bit, height_key, touch_key,
};

/// The test map: 8 texels a side of 10 mm, texel 4 starting at local x 0.
const TEXEL: f32 = 0.01;
const SIDE: u32 = 8;
const ORIGIN: [f32; 2] = [-0.04, 0.0];
/// `WM_DRY_SECONDS` in `wet_map_layout.wgsl`.
const DRY_SECONDS: f64 = 15.0;

/// The albedo the probe kernel passes in, one per channel, and its roughness.
const ALBEDO: [f32; 3] = [0.2, 0.5, 0.8];
const ROUGHNESS: f32 = 0.6;

/// Evaluates the look at the points in `probe_in`: (albedo rgb, roughness).
const WET_KERNEL: &str = "\n\
@group(0) @binding(91) var<storage, read> probe_in: array<vec4<f32>>;\n\
@group(0) @binding(90) var<storage, read_write> probe_out: array<vec4<f32>>;\n\
@compute @workgroup_size(64)\n\
fn cs_probe(@builtin(global_invocation_id) id: vec3<u32>) {\n\
    let i = id.x;\n\
    if i >= arrayLength(&probe_out) { return; }\n\
    let surface = look_wet_surface(probe_in[i].xyz, vec3<f32>(0.2, 0.5, 0.8), 0.6);\n\
    probe_out[i] = vec4<f32>(surface.albedo, surface.roughness);\n\
}\n";

/// One touched texel: when, and the heights touched.
struct Touch {
    texel: [u32; 2],
    at: f32,
    heights: &'static [f32],
}

/// A map's words as `node.wet_map` writes them, placed at `ORIGIN` with
/// `texel`, at display time `now`.
fn map_words(texel: f32, now: f32, drying: f32, touches: &[Touch]) -> Vec<u32> {
    let texels = u64::from(SIDE * SIDE);
    let mut words = vec![0; (HEADER_WORDS + TEXEL_WORDS * texels) as usize];
    words[..6].copy_from_slice(&[
        ORIGIN[0].to_bits(),
        ORIGIN[1].to_bits(),
        texel.to_bits(),
        SIDE,
        now.to_bits(),
        drying.to_bits(),
    ]);
    for touch in touches {
        let at = HEADER_WORDS + TEXEL_WORDS * u64::from(touch.texel[1] * SIDE + touch.texel[0]);
        let at = at as usize;
        let low = touch.heights.iter().copied().fold(f32::INFINITY, f32::min);
        let high = touch
            .heights
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        words[at] = touch_key(touch.at);
        words[at + 1] = !height_key(low);
        words[at + 2] = height_key(high);
        words[at + 3] = touch
            .heights
            .iter()
            .fold(0, |bins, &y| bins | height_bit(y, 2.0 * texel));
    }
    words
}

/// Water ran down a wall at local x 0 from 0.3 m to 0.1 m at time 10.
const DOWN_THE_WALL: Touch = Touch {
    texel: [4, 2],
    at: 10.0,
    heights: &[0.1, 0.13, 0.16, 0.19, 0.22, 0.25, 0.28, 0.3],
};

/// A splash at a wall's foot and one at its top, nothing between.
const FOOT_AND_TOP: Touch = Touch {
    texel: [2, 5],
    at: 10.0,
    heights: &[0.1, 0.4],
};

/// The model of the test water: moved by (2, 0, -1).
fn water_model() -> [[f32; 4]; 4] {
    model_matrix([2.0, 0.0, -1.0], [0.0; 3], [1.0; 3])
}

/// The world point at height `y` over texel coordinate (`u`, `v`) of the test
/// map, in texels from its low corner.
fn over(u: f32, v: f32, y: f32) -> [f32; 4] {
    let x = ORIGIN[0] + u * TEXEL;
    let z = ORIGIN[1] + v * TEXEL;
    [2.0 + x, y, -1.0 + z, 0.0]
}

/// The look at `points` with `uniforms`, reading the wet map `map`.
fn probe(
    device: &GpuDevice,
    uniforms: &WetUniforms,
    map: &GpuBuffer,
    points: &[[f32; 4]],
) -> Vec<[f32; 4]> {
    let wgsl = format!(
        "{LAYOUT}{}{WET_KERNEL}",
        include_str!("../shaders/look_wet_surface.wgsl")
    );
    let pipeline = device.create_compute_pipeline(&wgsl, "cs_probe", "wet probe");
    let flat: Vec<f32> = points.iter().flatten().copied().collect();
    let input = shared_buffer(device, &flat);
    let out = device.create_buffer_shared((points.len() * 16) as u64);
    let mut enc = device.create_encoder("wet probe");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 68,
                data: bytemuck::bytes_of(uniforms),
            },
            GpuBinding::Buffer {
                binding: 69,
                buffer: map,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 90,
                buffer: &out,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 91,
                buffer: &input,
                offset: 0,
            },
        ],
        [points.len().div_ceil(64) as u32, 1, 1],
        "wet probe",
    );
    enc.commit_and_wait_completed();
    let values: &[f32] = unsafe {
        std::slice::from_raw_parts(
            out.mapped_ptr().expect("shared").cast::<f32>(),
            points.len() * 4,
        )
    };
    values
        .chunks_exact(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect()
}

/// The soaked albedo of `look_wet_surface.wgsl`, in f64.
fn soaked(albedo: f64) -> f64 {
    let (n, r_e) = (1.333f64, 0.066f64);
    let r_i = 1.0 - (1.0 - r_e) / (n * n);
    (1.0 - r_e) * (1.0 - r_i) * albedo / (1.0 - r_i * albedo)
}

/// The look's albedo and roughness at wetness `w`.
fn look_at(w: f64) -> [f64; 4] {
    let blend = |a: f32| {
        let a = f64::from(a);
        a + w * (soaked(a) - a)
    };
    [
        blend(ALBEDO[0]),
        blend(ALBEDO[1]),
        blend(ALBEDO[2]),
        (f64::from(ROUGHNESS) * (1.0 - w)).max(0.01),
    ]
}

fn assert_wetness(point: [f32; 4], w: f64, what: &str) {
    for (channel, want) in look_at(w).into_iter().enumerate() {
        assert!(
            (f64::from(point[channel]) - want).abs() < 1e-5,
            "{what}: wetness {w}, channel {channel} is {} against {want}",
            point[channel]
        );
    }
}

/// The uniforms of a scene whose water carries `map`.
fn bound(device: &GpuDevice, map: &GpuBuffer) -> WetUniforms {
    let stand_in = device.create_buffer_shared(16);
    let object = FieldObject {
        slot: 0,
        levelset: &stand_in,
        velocity: None,
        foam: None,
        mist: None,
        wet_map: Some(map),
        tiles: &stand_in,
        extent: &stand_in,
        pool_tiles: 1,
        cell_size: 0.0625,
        model: water_model(),
        prev_model: water_model(),
        ripple_detail: 1.0,
        uniforms: bytemuck::Zeroable::zeroed(),
        mesh: None,
    };
    wet_uniforms(Some(&object)).expect("a real model inverts")
}

#[test]
fn a_touched_texel_is_wet_between_its_heights_and_dry_elsewhere() {
    let device = manifold_gpu::testkit::test_device();
    let map = shared_buffer(
        &device,
        &map_words(TEXEL, 15.0, 1.0, &[DOWN_THE_WALL, FOOT_AND_TOP]),
    );
    let on = bound(&device, &map);
    let off = wet_uniforms(None).expect("off is not an error");
    let five_seconds = (-5.0 / DRY_SECONDS).exp();
    let wet = [
        // The texel's centre, halfway up the run, and just inside the two
        // texels of margin above and below it.
        over(4.5, 2.5, 0.2),
        over(4.5, 2.5, 0.3 + 1.5 * TEXEL),
        over(4.5, 2.5, 0.1 - 1.5 * TEXEL),
        // The foot and the top of a wall splashed at both.
        over(2.5, 5.5, 0.1),
        over(2.5, 5.5, 0.4),
    ];
    // Halfway to the untouched texel beside it: only the touched one counts,
    // as on a wall's face.
    let edge = over(5.0, 2.5, 0.2);
    let dry = [
        // Above and below the run.
        over(4.5, 2.5, 0.6),
        over(4.5, 2.5, 0.1 - 2.5 * TEXEL),
        // Halfway up the wall splashed only at its foot and top.
        over(2.5, 5.5, 0.25),
        // An untouched texel, and off the map.
        over(6.5, 2.5, 0.2),
        over(-2.0, 2.5, 0.2),
        over(4.5, 9.5, 0.2),
    ];
    let points: Vec<[f32; 4]> = wet.into_iter().chain([edge]).chain(dry).collect();
    let looks = probe(&device, &on, &map, &points);
    for (i, &point) in looks.iter().take(wet.len()).enumerate() {
        assert_wetness(point, five_seconds, &format!("wet point {i}"));
    }
    assert_wetness(looks[wet.len()], five_seconds, "the edge");
    let unchanged = [ALBEDO[0], ALBEDO[1], ALBEDO[2], ROUGHNESS].map(f32::to_bits);
    for (i, point) in looks.iter().enumerate().skip(wet.len() + 1) {
        assert_eq!(point.map(f32::to_bits), unchanged, "dry point {i}");
    }
    // A scene without the map leaves every point alone.
    for (i, point) in probe(&device, &off, &map, &points).iter().enumerate() {
        assert_eq!(
            point.map(f32::to_bits),
            unchanged,
            "point {i} with the look off"
        );
    }
}

#[test]
fn a_touch_dries_by_its_age_and_the_drying_rate() {
    let device = manifold_gpu::testkit::test_device();
    let centre = [over(4.5, 2.5, 0.2)];
    for (now, drying) in [(70.0, 1.0), (15.0, 2.0), (10.0, 1.0)] {
        let map = shared_buffer(&device, &map_words(TEXEL, now, drying, &[DOWN_THE_WALL]));
        let look = probe(&device, &bound(&device, &map), &map, &centre)[0];
        let w = (-f64::from(now - 10.0) * f64::from(drying) / DRY_SECONDS).exp();
        assert_wetness(look, w, &format!("{} s at drying {drying}", now - 10.0));
    }
    // A minute on at Drying 1 is nearly dry.
    assert!((-60.0 / DRY_SECONDS).exp() < 0.02);
    // A map not yet placed is dry everywhere.
    let unplaced = shared_buffer(&device, &map_words(0.0, 15.0, 1.0, &[DOWN_THE_WALL]));
    let look = probe(&device, &bound(&device, &unplaced), &unplaced, &centre)[0];
    let unchanged = [ALBEDO[0], ALBEDO[1], ALBEDO[2], ROUGHNESS].map(f32::to_bits);
    assert_eq!(look.map(f32::to_bits), unchanged);
}
