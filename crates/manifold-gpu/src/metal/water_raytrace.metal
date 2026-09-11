// Secondary rays for the reconstructed liquid level set. Scene intersections
// share MANIFOLD's TLAS, material table, alpha test and hit-lighting helpers.
// The water boundary stays implicit; it is not inserted into the triangle TLAS.
struct WaterRayParams {
    float4x4 inv_view;
    float4 projection;
    float4 material;
    float4 attenuation;
    uint4 screen;
};

static float water_field(texture3d<float> density, float3 p) {
    float3 uv = (p - float3(-2, 0, -2)) * 0.25;
    if (any(uv < 0.0) || any(uv > 1.0)) return 0.0;
    constexpr sampler s(coord::normalized, address::clamp_to_edge, filter::linear);
    return density.sample(s, uv, level(0)).r;
}

static float3 water_outward_normal(texture3d<float> density, float3 p, float voxel, float3 fallback) {
    float3 g = float3(
        water_field(density, p + float3(voxel,0,0)) - water_field(density, p - float3(voxel,0,0)),
        water_field(density, p + float3(0,voxel,0)) - water_field(density, p - float3(0,voxel,0)),
        water_field(density, p + float3(0,0,voxel)) - water_field(density, p - float3(0,0,voxel)));
    return dot(g,g) > 1e-12 ? -normalize(g) : fallback;
}

// Entry is known to lie on/inside the same isosurface used by the primary
// pass. Half-voxel steps and eight bisections bound the exit-position error.
static float water_exit(texture3d<float> density, float3 origin, float3 direction,
                        float isovalue, float voxel, float stop_distance) {
    float step = voxel * 0.5;
    float t = 0.0;
    for (uint i = 0; i < 4096; ++i) {
        float next = min(t + step, stop_distance);
        if (water_field(density, origin + direction * next) < isovalue) {
            float a = t, b = next;
            for (uint j = 0; j < 8; ++j) {
                float mid = (a + b) * 0.5;
                if (water_field(density, origin + direction * mid) >= isovalue) a = mid;
                else b = mid;
            }
            return (a + b) * 0.5;
        }
        if (next >= stop_distance) return stop_distance;
        t = next;
    }
    return stop_distance;
}

static float water_schlick(float cosine, float ior) {
    float f = (ior - 1.0) / (ior + 1.0);
    return f*f + (1.0-f*f) * pow(1.0-clamp(cosine,0.0,1.0),5.0);
}

struct WaterSceneHit { float3 radiance; float distance; };

static WaterSceneHit water_scene_ray(
    instance_acceleration_structure accel,
    device RtNormalSource* sources, device GiMaterial* materials,
    array<texture2d<float>, MAX_RT_MATERIAL_TEXTURES> textures,
    texture2d<float> env, constant ShadowRayParams& p,
    float3 origin, float3 direction, float roughness, uint2 pixel,
    device RtTraceDiagnostics* diagnostics)
{
    ray r;
    r.origin = origin; r.direction = direction;
    r.min_distance = 0.0001; r.max_distance = 10000.0;
    intersection_query<triangle_data, instancing> q;
    q.reset(r, accel, RT_MASK_VISIBLE);
    if (!walk_with_alpha_test(q, sources, textures, false))
        return {refl_env_sample(env, direction, roughness), 10000.0};
    uint iid = q.get_committed_instance_id(), pid = q.get_committed_primitive_id();
    float2 bary = q.get_committed_triangle_barycentric_coord();
    float distance = q.get_committed_distance();
    device RtNormalSource& src = sources[iid];
    float3 albedo = float3(materials[iid].albedo);
    constexpr sampler s(coord::normalized, address::repeat, filter::linear);
    float2 uv = fetch_interpolated_uv(sources, iid, pid, bary);
    if (src.base_color_tex_index < MAX_RT_MATERIAL_TEXTURES)
        albedo *= textures[src.base_color_tex_index].sample(s,uv).rgb;
    float2 mr = materials[iid].metallic_roughness.xy;
    if (src.mr_tex_index < MAX_RT_MATERIAL_TEXTURES)
        mr *= textures[src.mr_tex_index].sample(s,uv).bg;
    float3 n = fetch_interpolated_normal(sources, iid, pid, bary);
    if (dot(n,direction) > 0.0) n = -n;
    float3 hp = origin + direction * distance;
    float3 emission = emissive_at_hit(float3(materials[iid].emissive),src,sources,textures,iid,pid,bary);
    float3 f0 = mix(float3(0.04), albedo, mr.x);
    float3 diffuse = albedo * (1.0-mr.x) * refl_env_sample(env,n,1.0);
    float3 specular = f0 * refl_env_sample(env,reflect(direction,n),mr.y);
    float3 sun = sun_bounce_at_hit(accel,sources,materials,textures,p,min(p.caster_count,MAX_RT_CASTERS),
        hp,n,albedo*(1.0-mr.x),0.0002,pixel,900u,diagnostics);
    return {emission + diffuse + specular + sun, distance};
}

kernel void trace_water_rays(
    instance_acceleration_structure accel [[buffer(0)]],
    constant ShadowRayParams& p [[buffer(1)]],
    device GiMaterial* materials [[buffer(2)]],
    device RtNormalSource* sources [[buffer(3)]],
    constant WaterRayParams& w [[buffer(4)]],
    device RtTraceDiagnostics* diagnostics [[buffer(5)]],
    constant uint4& region [[buffer(8)]],
    texture2d<float,access::read> depth [[texture(0)]],
    texture2d<float,access::read> normals [[texture(1)]],
    texture3d<float> density [[texture(2)]],
    depth2d<float> opaque_depth [[texture(3)]],
    array<texture2d<float>,MAX_RT_MATERIAL_TEXTURES> textures [[texture(4)]],
    texture2d<float> env [[texture(68)]],
    texture2d<float,access::write> reflection [[texture(69)]],
    texture2d<float,access::write> transmission [[texture(70)]],
    uint2 local [[thread_position_in_grid]])
{
    if (any(local >= region.zw)) return;
    uint2 pixel = region.xy + local;
    if (any(pixel >= w.screen.xy)) return;
    reflection.write(float4(0),pixel); transmission.write(float4(0),pixel);
    float raw = depth.read(pixel).r;
    float4 nv = normals.read(pixel);
    if (nv.a < 0.5 || raw >= 1.0 || raw > opaque_depth.read(pixel) ||
        !all(isfinite(nv)) || !isfinite(raw) || dot(nv.xyz,nv.xyz) < 1e-12) return;
    float near = w.projection.x, far = w.projection.y;
    float z = near*far/(far-raw*(far-near));
    float2 uv = (float2(pixel)+0.5)/float2(w.screen.xy);
    float3 vp = float3((uv.x*2-1)*w.projection.z*w.projection.w*z,
                       (1-uv.y*2)*w.projection.z*z,-z);
    float3 origin = (w.inv_view * float4(vp,1)).xyz;
    float3 incident = normalize((w.inv_view*float4(vp,0)).xyz);
    float3 normal = normalize((w.inv_view*float4(nv.x,nv.y,-nv.z,0)).xyz);
    if (dot(normal,incident) > 0.0) normal = -normal;
    device RtNormalSource* ss = sources+p.slot_row_base;
    device GiMaterial* sm = materials+p.slot_row_base;
    float voxel = 4.0/float(max(density.get_width(),max(density.get_height(),density.get_depth())));
    float bias = voxel/1024.0;
    float3 reflected_dir = reflect(incident,normal);
    // Smooth water is deterministic. Rough water reuses the engine's GGX
    // direction sampler; no temporal reuse of moving liquid geometry.
    if (w.material.y > 0.0) {
        float3 candidate = ggx_reflection_dir(normal,-incident,w.material.y,
            blue_noise_sample(pixel,p.frame_index,0,1));
        if (dot(candidate,normal)>0.0) reflected_dir=candidate;
    }
    WaterSceneHit reflected = water_scene_ray(accel,ss,sm,textures,env,p,
        origin+normal*bias,reflected_dir,w.material.y,pixel,diagnostics);
    float sun_visibility = 1.0;
    for (uint c=0;c<min(p.caster_count,MAX_RT_CASTERS);++c) {
        if(p.casters[c].kind!=0u) continue;
        ray sr; sr.origin=origin+normal*bias; sr.direction=normalize(float3(p.casters[c].dir_or_pos));
        sr.min_distance=bias; sr.max_distance=10000.0;
        intersection_query<triangle_data,instancing> sq;
        sq.reset(sr,accel,RT_MASK_SHADOW_CASTER);
        sun_visibility=walk_with_alpha_test(sq,ss,textures,true)?0.0:1.0;
        break;
    }
    reflection.write(float4(reflected.radiance,sun_visibility),pixel);
    float3 direction = refract(incident,normal,1.0/w.material.x);
    float3 sigma = -log(clamp(w.attenuation.rgb,float3(1e-4),float3(1)))/max(w.material.z,1e-3);
    float3 weight = float3(1), radiance = float3(0);
    float travelled = 0.0;
    // Four dielectric interfaces retain reflected energy inside the liquid,
    // including total internal reflection. Residual energy is truncated at
    // this explicit bounce limit, like the engine's other secondary rays.
    for(uint bounce=0;bounce<4;++bounce) {
        WaterSceneHit hit = water_scene_ray(accel,ss,sm,textures,env,p,
            origin+direction*bias,direction,w.material.y,pixel,diagnostics);
        float distance = water_exit(density,origin,direction,w.material.w,voxel,min(hit.distance+bias,8.0));
        travelled += distance;
        weight *= exp(-sigma*distance);
        if(hit.distance+bias <= distance+1e-5) {
            radiance += weight*hit.radiance;
            break;
        }
        origin += direction*distance;
        float3 outward = water_outward_normal(density,origin,voxel,direction);
        if(dot(outward,direction)<0.0) outward=-outward;
        float3 escaped = refract(direction,-outward,w.material.x);
        float f = 1.0;
        if(dot(escaped,escaped)>0.5) {
            f=water_schlick(dot(outward,direction),w.material.x);
            WaterSceneHit outside = water_scene_ray(accel,ss,sm,textures,env,p,
                origin+outward*bias,escaped,w.material.y,pixel,diagnostics);
            radiance += weight*(1.0-f)*outside.radiance;
        }
        weight *= f;
        if(max(weight.x,max(weight.y,weight.z))<1e-4) break;
        direction=reflect(direction,outward);
        origin-=outward*bias;
    }
    transmission.write(float4(radiance,travelled),pixel);
}
