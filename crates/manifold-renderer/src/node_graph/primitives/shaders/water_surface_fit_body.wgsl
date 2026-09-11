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

struct Fit { mean: vec3<f32>, cov: mat3x3<f32>, neighbours: u32 }
fn fit(idx:u32,count:u32)->Fit {
  let c=buf_particles[idx].position_mass.xyz; var mean=vec3<f32>(0.); var second=mat3x3<f32>(vec3<f32>(0.),vec3<f32>(0.),vec3<f32>(0.)); var wsum=0.; var found=0u;
  let cell=vec3<i32>(floor(c/0.125+vec3<f32>(16.,0.,16.)));
  for(var dz=-1;dz<=1;dz++){for(var dy=-1;dy<=1;dy++){for(var dx=-1;dx<=1;dx++){
    let q=cell+vec3<i32>(dx,dy,dz); if(any(q<vec3<i32>(0))||any(q>=vec3<i32>(32))){continue;}
    var link=buf_heads[u32((q.z*32+q.y)*32+q.x)]; var steps=0u;
    loop { if(link==0u||steps>=count){break;} let j=link-1u; if(j>=count){break;} let d=buf_particles[j].position_mass.xyz-c; let d2=dot(d,d); if(j!=idx&&d2<0.125*0.125){let x=1.-d2/(0.125*0.125);let w=x*x*x;mean+=d*w;second+=mat3x3<f32>(d*d.x*w,d*d.y*w,d*d.z*w);wsum+=w;found+=1u;} link=buf_next[j];steps+=1u;}
  }}}
  if(wsum>0.){mean/=wsum;second*=1.0/wsum;} let cov=second-mat3x3<f32>(mean*mean.x,mean*mean.y,mean*mean.z); return Fit(mean,cov,found);
}

fn body(idx:u32,count:u32,radius:f32,blend:f32)->Element2 {
 let p=buf_particles[idx];let c=p.position_mass.xyz;
 if(p.position_mass.w==0.0 || any((bitcast<vec3<u32>>(c)&vec3<u32>(0x7f800000u))==vec3<u32>(0x7f800000u))){return Element2(vec4<f32>(0.),vec4<f32>(0.),vec4<f32>(0.),vec4<f32>(0.));}
 let f=fit(idx,count);
 if(f.neighbours<8u){return Element2(vec4<f32>(c,radius),vec4<f32>(radius,0.,0.,0.),vec4<f32>(0.,radius,0.,0.),vec4<f32>(0.,0.,radius,0.));}
 let eigen=symmetric_eigen3_jacobi(f.cov);
 let floor_value=max(max(eigen.values.x,max(eigen.values.y,eigen.values.z))*0.25,1e-8);
 let values=max(eigen.values,vec3<f32>(floor_value));
 let sizes=radius*values/pow(values.x*values.y*values.z,1./3.);
 let center=c+clamp(blend,0.,1.)*f.mean;
 return Element2(vec4<f32>(center,max(sizes.x,max(sizes.y,sizes.z))),vec4<f32>(eigen.vectors[0]*sizes.x,0.),vec4<f32>(eigen.vectors[1]*sizes.y,0.),vec4<f32>(eigen.vectors[2]*sizes.z,0.));
}
