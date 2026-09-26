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
        // Scene textures and the layer compositor use premultiplied alpha.
        // Preserve that convention through reconstruction: unpremultiplying
        // here makes faint optical coverage display as solid rims (BUG-imds).
        var original_pm = original;

        // The half-resolution gather has no support below one source pixel.
        // A bounded full-resolution disc bridges that gap for camera alpha
        // blur. Keep taps on the same original CoC surface so a focused
        // foreground cannot pull in an unrelated far colour. Feed this
        // filtered value into the existing reconstruction below; near
        // coverage must not be composited over the same foreground twice.
        let small_blur = radius >= 0.5 && radius < 2.0;
        if small_blur {
            let source_dims = textureDimensions(source);
            let coc_dims = textureDimensions(coc);
            var small_rgb = vec3<f32>(0.0);
            var small_alpha = 0.0;
            var small_weight = 0.0;
            for(var oy:i32=-1;oy<=1;oy++) {
                for(var ox:i32=-1;ox<=1;ox++) {
                    let sp = clamp(vec2<i32>(id.xy)+vec2<i32>(ox,oy),vec2<i32>(0),vec2<i32>(source_dims)-1);
                    let sample = textureLoad(source,sp,0);
                    let sample_cp = min(vec2<u32>((vec2<f32>(sp)+0.5)*vec2<f32>(coc_dims)/vec2<f32>(source_dims)),coc_dims-1u);
                    let sample_z = textureLoad(coc,vec2<i32>(sample_cp),0);
                    let sample_radius = clamp(sample_z.r,0.0,1.0)*u.radius;
                    let sample_near = sample_z.g >= 0.5;
                    let signed_radius = select(radius,-radius,is_near);
                    let sample_signed_radius = select(sample_radius,-sample_radius,sample_near);
                    // A continuous signed-CoC comparison also spans the
                    // focus plane without a categorical near/far jump.
                    let compatibility = 1.0-smoothstep(0.5,1.5,abs(sample_signed_radius-signed_radius));
                    let distance = length(vec2<f32>(f32(ox),f32(oy)));
                    let weight = clamp(radius-distance+0.5,0.0,1.0)*compatibility;
                    // Transparent texels can retain undefined RGB in source
                    // textures; only premultiplied coverage may contribute.
                    let sample_rgb = select(vec3<f32>(0.0),sample.rgb,sample.a > 1e-6);
                    small_rgb += sample_rgb*weight;
                    small_alpha += sample.a*weight;
                    small_weight += weight;
                }
            }
            original_pm = vec4<f32>(small_rgb/max(small_weight,1e-6),small_alpha/max(small_weight,1e-6));
        }
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

        if radius <= 0.5 && near_rgba.a == 0.0 && (original.a > 0.0 || result.a == 0.0) {
            textureStore(output_tex,vec2<i32>(id.xy),original);
        } else {
            textureStore(output_tex,vec2<i32>(id.xy),vec4<f32>(result.rgb,clamp(result.a,0.0,1.0)));
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
