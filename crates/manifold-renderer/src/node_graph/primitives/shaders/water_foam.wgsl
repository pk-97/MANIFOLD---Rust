struct Uniforms { count: u32, dt: f32, gain: f32, decay: f32 }
struct Particle { position_mass: vec4<f32>, velocity_density: vec4<f32>, affine_x: vec4<f32>, affine_y: vec4<f32>, affine_z: vec4<f32>, previous_position: vec4<f32> }
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var<storage, read> particles: array<Particle>;
@group(0) @binding(2) var<storage, read> previous: array<f32>;
@group(0) @binding(3) var<storage, read_write> foam: array<f32>;
@compute @workgroup_size(256)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
  let i = id.x; if (i >= u.count) { return; }
  let p = particles[i];
  if (p.position_mass.w == 0.0) { foam[i] = 0.0; return; }
  if (u.dt == 0.0) { foam[i] = previous[i]; return; }
  let c = mat3x3<f32>(p.affine_x.xyz, p.affine_y.xyz, p.affine_z.xyz);
  let s = 0.5 * (c + transpose(c));
  let d = s - mat3x3<f32>(vec3<f32>(1.0,0.0,0.0), vec3<f32>(0.0,1.0,0.0), vec3<f32>(0.0,0.0,1.0)) * ((s[0][0]+s[1][1]+s[2][2])/3.0);
  let strain = sqrt(max(0.0, dot(d[0],d[0])+dot(d[1],d[1])+dot(d[2],d[2])));
  let speed = length(p.velocity_density.xyz);
  let source = u.gain * smoothstep(2.0, 10.0, strain) * smoothstep(0.15, 1.0, speed);
  let equilibrium = source / (source + u.decay);
  foam[i] = clamp(equilibrium + (previous[i] - equilibrium) * exp(-(source + u.decay) * u.dt), 0.0, 1.0);
}
