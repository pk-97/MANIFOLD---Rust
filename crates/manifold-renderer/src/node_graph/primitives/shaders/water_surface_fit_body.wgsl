// Symmetric 3x3 eigendecomposition by five fixed Jacobi sweeps.
// Matrix and eigenvector columns follow WGSL's column-major indexing.
struct SymmetricEigen3 {
  values: vec3<f32>,
  vectors: mat3x3<f32>,
};

fn symmetric_eigen3_jacobi(covariance: mat3x3<f32>) -> SymmetricEigen3 {
  var a: array<vec3<f32>, 3> = array<vec3<f32>, 3>(
    covariance[0], covariance[1], covariance[2]
  );
  var v: array<vec3<f32>, 3> = array<vec3<f32>, 3>(
    vec3<f32>(1.0, 0.0, 0.0),
    vec3<f32>(0.0, 1.0, 0.0),
    vec3<f32>(0.0, 0.0, 1.0)
  );

  for (var sweep: u32 = 0u; sweep < 5u; sweep++) {
    // A fixed ordering is deterministic and covers every off-diagonal pair.
    for (var pair: u32 = 0u; pair < 3u; pair++) {
      var p: u32 = 0u;
      var q: u32 = 1u;
      var r_index: u32 = 2u;
      if (pair == 1u) { p = 0u; q = 2u; r_index = 1u; }
      if (pair == 2u) { p = 1u; q = 2u; r_index = 0u; }

      let apq = a[q][p];
      if (abs(apq) < 1e-12) {
        continue;
      }

      let app = a[p][p];
      let aqq = a[q][q];
      let tau = (aqq - app) / (2.0 * apq);
      let sign_tau = select(-1.0, 1.0, tau >= 0.0);
      let t = sign_tau / (abs(tau) + sqrt(1.0 + tau * tau));
      let c = 1.0 / sqrt(1.0 + t * t);
      let s = t * c;

      // Apply J^T A J while retaining symmetry. The remaining index is r.
      let arp = a[p][r_index];
      let arq = a[q][r_index];
      let new_arp = c * arp - s * arq;
      let new_arq = s * arp + c * arq;
      a[p][r_index] = new_arp;
      a[r_index][p] = new_arp;
      a[q][r_index] = new_arq;
      a[r_index][q] = new_arq;

      a[p][p] = c * c * app - 2.0 * s * c * apq + s * s * aqq;
      a[q][q] = s * s * app + 2.0 * s * c * apq + c * c * aqq;
      a[p][q] = 0.0;
      a[q][p] = 0.0;

      let vp = v[p];
      let vq = v[q];
      v[p] = c * vp - s * vq;
      v[q] = s * vp + c * vq;
    }
  }

  return SymmetricEigen3(
    vec3<f32>(a[0][0], a[1][1], a[2][2]),
    mat3x3<f32>(v[0], v[1], v[2])
  );
}

// Yu & Turk (2010), equations 6 and 9–16. h is the cubic-spline scale;
// both the covariance neighbourhood and isotropic support have radius 2h.
struct Fit { mean: vec3<f32>, cov: mat3x3<f32>, neighbours: u32, density: f32 }
fn fit(idx: u32, count: u32, h: f32) -> Fit {
  let center = buf_particles[idx].position_mass.xyz;
  let support = 2.0*h;
  let origin = vec3<f32>(-2.0, 0.0, -2.0);
  let lo = max(vec3<i32>(floor((center-vec3<f32>(support)-origin)/0.125)), vec3<i32>(0));
  let hi = min(vec3<i32>(floor((center+vec3<f32>(support)-origin)/0.125)), vec3<i32>(31));
  var mean = vec3<f32>(0.0);
  var second = mat3x3<f32>(vec3<f32>(0.0),vec3<f32>(0.0),vec3<f32>(0.0));
  var weight_sum = 0.0;
  var found = 0u;
  var density = 0.0;
  let norm = 1.0/(3.14159265359*h*h*h);
  for (var z=lo.z; z<=hi.z; z++) { for (var y=lo.y; y<=hi.y; y++) { for (var x=lo.x; x<=hi.x; x++) {
    var link = buf_heads[u32(x+32*(y+32*z))];
    var steps = 0u;
    loop {
      if (link==0u || steps>=count) { break; }
      let j=link-1u;
      if (j>=count) { break; }
      let p=buf_particles[j];
      link=buf_next[j];
      steps++;
      if (p.position_mass.w<=0.0) { continue; }
      let d=p.position_mass.xyz-center;
      let distance=length(d);
      if (distance<support) {
        // Eq.17 restricts the geometric fit, not Eq.1's SPH density.
        // The one-word unwired sentinel cannot cover a multi-particle array;
        // for a single particle its label is immaterial (self is included).
        let components_present = arrayLength(&buf_components) >= count;
        var same_component = true;
        if (components_present) { same_component = buf_components[j] == buf_components[idx]; }
        if (same_component) {
          let q=distance/support;
          let w=1.0-q*q*q;
          mean+=d*w;
          second+=mat3x3<f32>(d*d.x*w,d*d.y*w,d*d.z*w);
          weight_sum+=w;
          found++;
        }
        // Becker–Teschner cubic spline, support q_h < 2.
        let q_h=distance/h;
        var cubic=0.25*pow(2.0-q_h,3.0);
        if (q_h<1.0) { cubic=1.0-1.5*q_h*q_h+0.75*q_h*q_h*q_h; }
        density+=p.position_mass.w*norm*cubic;
      }
    }
  }}}
  if (weight_sum==0.0) { return Fit(vec3<f32>(0.0),second,0u,0.0); }
  mean/=weight_sum;
  second*=1.0/weight_sum;
  let cov=second-mat3x3<f32>(mean*mean.x,mean*mean.y,mean*mean.z);
  return Fit(mean,cov,found,density);
}

fn body(idx: u32, count: u32, radius: f32, blend: f32) -> Element2 {
  let p=buf_particles[idx];
  let c=p.position_mass.xyz;
  if (p.position_mass.w<=0.0 || any((bitcast<vec3<u32>>(c)&vec3<u32>(0x7f800000u))==vec3<u32>(0x7f800000u))) {
    return Element2(vec4<f32>(0.0),vec4<f32>(0.0),vec4<f32>(0.0),vec4<f32>(0.0));
  }
  let h=radius;
  let f=fit(idx,count,h);
  let displacement=clamp(blend,0.0,1.0)*f.mean;
  let center=c+displacement;
  // Store FULL support semiaxes, i.e. columns of 2 G^-1. The .w lanes
  // carry reconstruction density and the conservative original-bin reach.
  let eigen=symmetric_eigen3_jacobi(f.cov);
  let largest=max(eigen.values.x,max(eigen.values.y,eigen.values.z));
  // Exact zero covariance (coincident samples) has no invertible fit.
  // Use the same finite spherical kernel as the sparse case.
  if (f.neighbours<=25u || largest<=0.0) {
    return Element2(vec4<f32>(center,h),vec4<f32>(h,0.0,0.0,f.density),
      vec4<f32>(0.0,h,0.0,length(displacement)+h),vec4<f32>(0.0,0.0,h,0.0));
  }
  // N>25 distinct samples is the fit's nondegenerate domain. This relative
  // floor is scale invariant; no fixed metre-space epsilon is introduced.
  let values=max(eigen.values,vec3<f32>(largest*0.25));
  // The paper calibrates ks so a full interior neighbourhood is unchanged,
  // but gives no metric scale for its example 1400. For w=1-(r/R)^3 on a
  // uniform 3D ball, each covariance eigenvalue is 3 R^2 / 20.
  let support=2.0*h;
  let ks=20.0/(3.0*support*support);
  let sizes=2.0*h*ks*values;
  let maximum=max(sizes.x,max(sizes.y,sizes.z));
  return Element2(vec4<f32>(center,maximum),vec4<f32>(eigen.vectors[0]*sizes.x,f.density),
    vec4<f32>(eigen.vectors[1]*sizes.y,length(displacement)+maximum),vec4<f32>(eigen.vectors[2]*sizes.z,0.0));
}
