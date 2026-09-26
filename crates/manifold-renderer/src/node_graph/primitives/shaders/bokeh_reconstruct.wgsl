struct Uniforms { radius: f32, blur_alpha: u32, _pad0: f32, _pad1: f32 }
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var coc: texture_2d<f32>;
@group(0) @binding(3) var guide: texture_2d<f32>;
@group(0) @binding(4) var far: texture_2d<f32>;
@group(0) @binding(5) var near: texture_2d<f32>;
@group(0) @binding(6) var samp: sampler;
@group(0) @binding(7) var output_tex: texture_storage_2d<rgba16float, write>;
@compute @workgroup_size(16,16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if any(id.xy >= dims) { return; }
    let uv = (vec2<f32>(id.xy)+0.5)/vec2<f32>(dims);
    let original = textureLoad(source,vec2<i32>(id.xy),0);
    if u.radius <= 0.0 {
        textureStore(output_tex,vec2<i32>(id.xy),original);
        return;
    }
    let coc_dims = textureDimensions(coc);
    let cp = min(vec2<u32>(uv*vec2<f32>(coc_dims)),coc_dims-1u);
    let z = textureLoad(coc,vec2<i32>(cp),0);
    let radius = clamp(z.r,0.0,1.0)*u.radius;
    let is_near = z.g >= 0.5;
    let half_dims = textureDimensions(guide);
    let half_p = (vec2<f32>(id.xy)+0.5)*0.5-0.5;
    let p0 = vec2<i32>(floor(half_p));
    let f = fract(half_p);
    var far_rgb = vec3<f32>(0.0);
    var far_weight = 0.0;
    var far_support = 0.0;
    var far_reach = 0.0;
    var near_rgba = vec4<f32>(0.0);
    for(var y=0;y<2;y++) {
        for(var x=0;x<2;x++) {
            let p = clamp(p0+vec2<i32>(x,y),vec2<i32>(0),vec2<i32>(half_dims)-1);
            let bilinear = select(1.0-f.x,f.x,x==1)*select(1.0-f.y,f.y,y==1);
            let g = textureLoad(guide,p,0);
            far_reach = max(far_reach,g.b*u.radius);
            let far_sample = textureLoad(far,p,0);
            // Radius discontinuities reject unrelated far surfaces. Near
            // pixels need a background fill, so accept available far samples.
            let radius_weight = select(1.0/(1.0+abs(g.r*u.radius-radius)),1.0,is_near);
            let weight = bilinear*radius_weight*far_sample.a;
            far_rgb += far_sample.rgb*weight;
            far_weight += weight;
            far_support += bilinear*radius_weight;
            // Foreground coverage must be allowed to cross depth edges.
            near_rgba += textureLoad(near,p,0)*bilinear;
        }
    }
    // Half-resolution gathers resolve radii of at least one source pixel.
    // Blend only above that support; smaller near radii have no gathered
    // coverage and must not fade the original silhouette toward transparent.
    let blur_amount = smoothstep(1.0,2.0,radius);
    if u.blur_alpha != 0u {
        // Straight-alpha scene output needs optical coverage beyond the
        // original silhouette. Composite in premultiplied space, then undo
        // premultiplication once; hidden transparent RGB never enters a mip.
        let original_pm = vec4<f32>(original.rgb*original.a,original.a);
        let far_pm = vec4<f32>(far_rgb,far_weight)/max(far_support,1e-6);
        var background = mix(original_pm,far_pm,blur_amount);
        // The half-resolution color filter alone must not expand a focused
        // silhouette. Only an actual defocused far footprint can add coverage
        // to a transparent full-resolution receiver.
        if original.a <= 1e-6 { background = far_pm*smoothstep(1.0,2.0,far_reach); }
        var result = near_rgba + background*(1.0-near_rgba.a);
        if is_near {
            result = mix(original_pm,near_rgba+far_pm*(1.0-near_rgba.a),blur_amount);
        }
        if blur_amount == 0.0 && near_rgba.a == 0.0 && (original.a > 0.0 || result.a == 0.0) {
            textureStore(output_tex,vec2<i32>(id.xy),original);
        } else {
            textureStore(output_tex,vec2<i32>(id.xy),vec4<f32>(result.rgb/max(result.a,1e-6),clamp(result.a,0.0,1.0)));
        }
        return;
    }
    var background = original.rgb;
    if far_weight > 1e-5 {
        background = mix(original.rgb,far_rgb/far_weight,blur_amount);
    } else if is_near && near_rgba.a > 1e-5 {
        background = mix(original.rgb,near_rgba.rgb/near_rgba.a,blur_amount);
    }
    // Sharp near texels keep their original detail. Outside that surface,
    // coverage already carries the neighboring foreground's actual blur.
    let near_mix = select(1.0,blur_amount,is_near);
    let rgb = near_rgba.rgb*near_mix + background*(1.0-near_rgba.a*near_mix);
    textureStore(output_tex,vec2<i32>(id.xy),vec4<f32>(rgb,original.a));
}
