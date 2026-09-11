// Yu–Turk Eq. 8 with Becker–Teschner's cubic spline. Shape axes are FULL
// support semiaxes, so q is unit-support and the normalizer is 8/(pi det A).
fn body(uv: vec3<f32>, dims: vec3<f32>, vol_res: i32, vol_depth: i32, radius: f32) -> vec4<f32> {
    let origin=vec3<f32>(-2.0,0.0,-2.0);
    let world=origin+uv*4.0;
    let r=radius;
    let shape_count=arrayLength(&buf_shapes);
    // The reduction covers actual variable-volume shapes and relocation.
    // Spherical unwired/zero-shape inputs retain their explicit rest density.
    let reach=max(r,bitcast<f32>(buf_reach[0]));
    let lo=max(vec3<i32>(floor((world-vec3<f32>(reach)-origin)/0.125)),vec3<i32>(0));
    let hi=min(vec3<i32>(floor((world+vec3<f32>(reach)-origin)/0.125)),vec3<i32>(31));
    var total=0.0;
    var foam_sum=0.0;
    for (var z=lo.z; z<=hi.z; z++) { for (var y=lo.y; y<=hi.y; y++) { for (var x=lo.x; x<=hi.x; x++) {
      let bin_min=origin+vec3<f32>(f32(x),f32(y),f32(z))*0.125;
      let nearest=clamp(world,bin_min,bin_min+vec3<f32>(0.125));
      if (dot(world-nearest,world-nearest)>reach*reach) { continue; }
      var link=buf_heads[u32(x+32*(y+32*z))];
      var guard=0u;
      loop {
        if (link==0u || guard>=arrayLength(&buf_particles)) { break; }
        let idx=link-1u;
        if (idx>=arrayLength(&buf_particles)) { break; }
        let p=buf_particles[idx];
        link=buf_next[idx];
        guard++;
        if (p.position_mass.w<=0.0) { continue; }
        var q=length(world-p.position_mass.xyz)/r;
        var determinant=r*r*r;
        var particle_density=1000.0;
        if (idx<shape_count) {
          let s=buf_shapes[idx];
          if (s.surface_center_radius.w>0.0) {
            let a=s.surface_axis_x.xyz;
            let b=s.surface_axis_y.xyz;
            let c=s.surface_axis_z.xyz;
            determinant=abs(dot(a,cross(b,c)));
            let delta=world-s.surface_center_radius.xyz;
            if (dot(delta,delta)>s.surface_center_radius.w*s.surface_center_radius.w) { continue; }
            q=length(vec3<f32>(dot(delta,a)/dot(a,a),dot(delta,b)/dot(b,b),dot(delta,c)/dot(c,c)));
            particle_density=s.surface_axis_x.w;
          }
        }
        if (q<1.0) {
          var cubic=2.0*pow(1.0-q,3.0);
          if (q<0.5) { cubic=1.0-6.0*q*q+6.0*q*q*q; }
          let w=p.position_mass.w/particle_density*8.0/(3.14159265359*determinant)*cubic;
          total+=w;
          foam_sum+=w*buf_foam[idx];
        }
      }
    }}}
    return vec4<f32>(total,foam_sum/max(total,1e-20),0.0,0.0);
}
