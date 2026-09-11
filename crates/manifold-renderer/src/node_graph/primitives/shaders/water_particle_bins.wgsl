// One linked list per .125m cell in the existing four-metre water domain.
struct U { count:u32, pad0:u32, pad1:u32, pad2:u32 }
struct P {
 position_mass:vec4<f32>, velocity_density:vec4<f32>,
 affine_x:vec4<f32>, affine_y:vec4<f32>, affine_z:vec4<f32>, previous_position:vec4<f32>,
}
@group(0) @binding(0) var<uniform> u:U;
@group(0) @binding(1) var<storage,read> p:array<P>;
@group(0) @binding(2) var<storage,read_write> h:array<atomic<u32>>;
@group(0) @binding(3) var<storage,read_write> n:array<u32>;
@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) id:vec3<u32>) {
 let i=id.x;
 if (i>=u.count) { return; }
 let particle=p[i]; let x=particle.position_mass.xyz;
 let nonfinite=any((bitcast<vec3<u32>>(x)&vec3<u32>(0x7f800000u))==vec3<u32>(0x7f800000u));
 let outside=any(x<vec3<f32>(-2.0,0.0,-2.0)) || any(x>=vec3<f32>(2.0,4.0,2.0));
 if (particle.position_mass.w==0.0 || nonfinite || outside) { return; }
 let cell=vec3<u32>(floor((x-vec3<f32>(-2.0,0.0,-2.0))/0.125));
 let index=(cell.z*32u+cell.y)*32u+cell.x;
 n[i]=atomicExchange(&h[index],i+1u);
}
