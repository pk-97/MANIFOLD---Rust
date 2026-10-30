// Half-resolution aperture gather. `in` is one premultiplied layer mip
// pyramid. `width` packs original far/near CoC in RG and tile reach in BA.
// Search bounds NEVER replace the original tap radii. No frame-dependent
// random rotation: subpixel motion samples a continuous, stable field.
// Independent golden-angle sets for each quality avoid angular aliasing from striding.
const BOKEH_OFFSETS: array<vec2<f32>,112> = array<vec2<f32>,112>(
    vec2<f32>(0.176776695, 0.000000000),
    vec2<f32>(-0.225772188, 0.206825818),
    vec2<f32>(0.034558052, -0.393771179),
    vec2<f32>(0.284571220, 0.371172764),
    vec2<f32>(-0.522223187, -0.092373929),
    vec2<f32>(0.494695392, -0.314684715),
    vec2<f32>(-0.165465927, 0.615525001),
    vec2<f32>(-0.315561468, -0.607594404),
    vec2<f32>(0.684642162, 0.250030219),
    vec2<f32>(-0.712256086, 0.294008958),
    vec2<f32>(0.343354499, -0.733728620),
    vec2<f32>(0.253730241, 0.808931990),
    vec2<f32>(-0.764745892, -0.443185876),
    vec2<f32>(0.897133984, -0.197232390),
    vec2<f32>(-0.547506905, 0.778772232),
    vec2<f32>(-0.126486773, -0.976089697),
    vec2<f32>(0.125000000, 0.000000000),
    vec2<f32>(-0.159645045, 0.146247939),
    vec2<f32>(0.024436233, -0.278438271),
    vec2<f32>(0.201222239, 0.262458779),
    vec2<f32>(-0.369267557, -0.065318231),
    vec2<f32>(0.349802466, -0.222515696),
    vec2<f32>(-0.117002079, 0.435241902),
    vec2<f32>(-0.223135654, -0.429634123),
    vec2<f32>(0.484115115, 0.176798064),
    vec2<f32>(-0.503641109, 0.207895728),
    vec2<f32>(0.242788294, -0.518824483),
    vec2<f32>(0.179414374, 0.572001296),
    vec2<f32>(-0.540757006, -0.313379738),
    vec2<f32>(0.634369523, -0.139464360),
    vec2<f32>(-0.387145845, 0.550675126),
    vec2<f32>(-0.089439655, -0.690199644),
    vec2<f32>(0.549071757, 0.462758258),
    vec2<f32>(-0.738878471, 0.030554945),
    vec2<f32>(0.538955125, -0.536332334),
    vec2<f32>(-0.036058186, 0.779791515),
    vec2<f32>(-0.512817532, -0.614526793),
    vec2<f32>(0.812359593, 0.109301834),
    vec2<f32>(-0.688310641, 0.478908616),
    vec2<f32>(0.188086057, -0.836061382),
    vec2<f32>(0.435033266, 0.759191055),
    vec2<f32>(-0.850448410, -0.271316239),
    vec2<f32>(0.826102401, -0.381680263),
    vec2<f32>(-0.357888202, 0.855155562),
    vec2<f32>(-0.319407334, -0.888033758),
    vec2<f32>(0.849908635, 0.446688159),
    vec2<f32>(-0.944034646, 0.248844503),
    vec2<f32>(0.536595808, -0.834529771),
    vec2<f32>(0.088388348, 0.000000000),
    vec2<f32>(-0.112886094, 0.103412909),
    vec2<f32>(0.017279026, -0.196885589),
    vec2<f32>(0.142285610, 0.185586382),
    vec2<f32>(-0.261111594, -0.046186964),
    vec2<f32>(0.247347696, -0.157342357),
    vec2<f32>(-0.082732964, 0.307762501),
    vec2<f32>(-0.157780734, -0.303797202),
    vec2<f32>(0.342321081, 0.125015110),
    vec2<f32>(-0.356128043, 0.147004479),
    vec2<f32>(0.171677249, -0.366864310),
    vec2<f32>(0.126865120, 0.404465995),
    vec2<f32>(-0.382372946, -0.221592938),
    vec2<f32>(0.448566992, -0.098616195),
    vec2<f32>(-0.273753452, 0.389386116),
    vec2<f32>(-0.063243386, -0.488044848),
    vec2<f32>(0.388252363, 0.327219502),
    vec2<f32>(-0.522465978, 0.021605608),
    vec2<f32>(0.381098824, -0.379244231),
    vec2<f32>(-0.025496988, 0.551395868),
    vec2<f32>(-0.362616754, -0.434536062),
    vec2<f32>(0.574424977, 0.077288068),
    vec2<f32>(-0.486709121, 0.338639530),
    vec2<f32>(0.132996927, -0.591184673),
    vec2<f32>(0.307614972, 0.536829143),
    vec2<f32>(-0.601357838, -0.191849552),
    vec2<f32>(0.584142610, -0.269888702),
    vec2<f32>(-0.253065174, 0.604686297),
    vec2<f32>(-0.225855092, -0.627934692),
    vec2<f32>(0.600976159, 0.315856226),
    vec2<f32>(-0.667533300, 0.175959636),
    vec2<f32>(0.379430535, -0.590101660),
    vec2<f32>(0.120699093, 0.702313483),
    vec2<f32>(-0.572007808, -0.442994998),
    vec2<f32>(0.731701930, -0.060620009),
    vec2<f32>(-0.505760271, 0.546712034),
    vec2<f32>(0.003684028, -0.755181387),
    vec2<f32>(0.514304945, 0.566946138),
    vec2<f32>(-0.772294866, -0.071576115),
    vec2<f32>(0.625787308, -0.474950256),
    vec2<f32>(-0.142380569, 0.782649522),
    vec2<f32>(-0.428883941, -0.681539482),
    vec2<f32>(0.785920133, 0.215388125),
    vec2<f32>(-0.733485521, 0.376412659),
    vec2<f32>(0.289861676, -0.781852102),
    vec2<f32>(0.317911469, 0.780941610),
    vec2<f32>(-0.770263923, -0.365042448),
    vec2<f32>(0.823263301, -0.253820875),
    vec2<f32>(-0.440156530, 0.751049086),
    vec2<f32>(-0.184643240, -0.859851367),
    vec2<f32>(0.724177356, 0.514421672),
    vec2<f32>(-0.890157361, 0.110938598),
    vec2<f32>(0.587054262, -0.689695435),
    vec2<f32>(0.033319900, 0.913688833),
    vec2<f32>(-0.647726957, -0.657276418),
    vec2<f32>(0.930014131, 0.047552248),
    vec2<f32>(-0.724323114, 0.598471827),
    vec2<f32>(0.130975323, -0.938766725),
    vec2<f32>(0.542204890, 0.787449273),
    vec2<f32>(-0.939649022, -0.216211044),
    vec2<f32>(0.845936750, -0.479273945),
    vec2<f32>(-0.302491897, 0.932435602),
    vec2<f32>(-0.410097094, -0.899101147),
    vec2<f32>(0.916975868, 0.389027966)
);
fn body(uv: vec2<f32>, dims: vec2<f32>, max_radius: f32, enabled: u32, quality: u32, blur_alpha: u32) -> vec4<f32> {
    let pixel = clamp(vec2<i32>(uv*dims),vec2<i32>(0),vec2<i32>(dims)-1);
    let guide = textureLoad(tex_width,pixel,0);
    let near_field = enabled == 1u;
    let center = textureSampleLevel(tex_in,samp,uv,0.0);
    let empty_far = blur_alpha != 0u && center.a < 1e-6 && guide.g == 0.0;
    // Far gather footprints follow the receiving surface. A tile-wide far
    // maximum chooses overly coarse color mips for small local CoC, producing
    // square blur patches where unrelated background enters the tile bound.
    // Near coverage must still search beyond its own silhouette.
    let far_search = select(max(guide.r,guide.g),guide.b,empty_far);
    let search_frac = select(far_search, guide.a, near_field);
    let radius = search_frac * max_radius;
    if radius < 0.5 {
        if near_field { return vec4<f32>(0.0); }
        return vec4<f32>(center.rgb / max(center.a,1e-6),center.a);
    }
    // Do not blur focused pixels just because distant tiles contain blur.
    // Near foreground pixels still gather a background fill for their halo.
    if !near_field && !empty_far && guide.r * max_radius < 0.5 && guide.g * max_radius < 0.5 {
        return vec4<f32>(center.rgb/max(center.a,1e-6),center.a);
    }
    let sample_count = 16u << min(quality,2u);
    let sample_base = sample_count - 16u;
    // Match the mip footprint to the area represented by one aperture tap.
    // radius/2 discarded the same detail at every quality and sampled broad
    // square patches even when enough taps were available to resolve them.
    let footprint = radius * sqrt(3.141592654 / f32(sample_count));
    let lod = max(log2(footprint),0.0);
    // LOOP-INVARIANT: guide lod/dims depend only on `lod`, computed above.
    let guide_lod = i32(min(ceil(lod),f32(textureNumLevels(tex_width)-1u)));
    let guide_dims = vec2<i32>(textureDimensions(tex_width,guide_lod));
    var rgb = vec3<f32>(0.0);
    var coverage = 0.0;
    var weights = 0.0;
    var aperture_weight = 0.0;
    for(var i=0u;i<sample_count;i++) {
        let offset = BOKEH_OFFSETS[sample_base+i] * radius;
        let tap_uv = uv + offset/dims;
        let sample = textureSampleLevel(tex_in, samp, tap_uv, lod);
        let distance = length(offset);
        aperture_weight += clamp(radius-distance+0.5,0.0,1.0);
        // Empty taps contribute nothing to rgb/coverage on the near_field or
        // blur_alpha paths (weights is unused there) and never need
        // tap_radius, so skip the 4-texel guide loads entirely.
        if (near_field || blur_alpha != 0u) && all(sample == vec4<f32>(0.0)) {
            continue;
        }
        // Match source-radius support to the coarse color footprint. Use a
        // conservative max over the bilinear 2x2 window; interpolating against
        // an empty texel would falsely shrink the radius of a thin foreground
        // silhouette. Coarse guide levels already hold each texel's 2x2 max
        // (bokeh_guide_downsample.wgsl), so only level zero loads four.
        let gp = vec2<i32>(floor(tap_uv*vec2<f32>(guide_dims)-0.5));
        var tap_guide = textureLoad(tex_width,clamp(gp,vec2<i32>(0),guide_dims-1),guide_lod);
        if guide_lod == 0 {
            for(var g=1;g<4;g++) {
                let o = vec2<i32>(g&1,g>>1u);
                tap_guide = max(tap_guide,textureLoad(tex_width,clamp(gp+o,vec2<i32>(0),guide_dims-1),0));
            }
        }
        let tap_radius = select(tap_guide.r,tap_guide.g,near_field)*max_radius;
        var weight = clamp(tap_radius-distance+0.5,0.0,1.0);
        if !near_field && guide.g * max_radius >= 0.5 {
            // Background extrapolation beneath defocused foreground.
            weight = 1.0;
        }
        if near_field {
            weight *= radius*radius/max(tap_radius*tap_radius,0.25);
        }
        rgb += sample.rgb * weight;
        coverage += sample.a * weight;
        weights += weight;
    }
    if near_field {
        let normalization = max(aperture_weight,1e-6);
        let alpha = coverage/normalization;
        return vec4<f32>(rgb/normalization/max(alpha,1.0),min(alpha,1.0));
    }
    // Transparent empty taps still occupy aperture area. Normalizing opacity
    // by accepted source taps would turn a faint halo into opaque geometry.
    let alpha_denominator = select(weights,aperture_weight,blur_alpha != 0u);
    return vec4<f32>(rgb/max(coverage,1e-6),min(coverage/max(alpha_denominator,1e-6),1.0));
}
