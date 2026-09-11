// Configurable MAC grid and translated world origin.
struct U{
    n:u32,active_count:u32,h:f32,ox:f32,oy:f32,oz:f32
}
@group(0)@binding(0)var<uniform>u:U;
struct P{
    pm:vec4<f32>,vd:vec4<f32>,cx:vec4<f32>,cy:vec4<f32>,cz:vec4<f32>,prev:vec4<f32>
}
@group(0)@binding(1)var<storage,read_write> p:array<P>;
@group(0)@binding(2)var<storage,read_write>a:array<atomic<i32>>;
@group(0)@binding(3)var<storage,read_write>s:array<atomic<u32>>;
@group(0)@binding(4)var<storage,read_write>g:array<vec4<f32>>;
fn fin(x:f32)->bool{
    return(bitcast<u32>(x)&0x7f800000u)!=0x7f800000u;
}
fn ws(f:f32)->vec3<f32>{
    return vec3(.5*(1.5-f)*(1.5-f),.75-(f-1.)*(f-1.),.5*(f-.5)*(f-.5));
}
fn ix(v:vec3<i32>)->u32{
    return u32(v.x)+u.n*(u32(v.y)+u.n*u32(v.z));
}
fn face_offset(a:u32)->vec3<f32>{
    if(a==0u){
        return vec3(0.,.5,.5);
    }if(a==1u){
        return vec3(.5,0.,.5);
    }return vec3(.5,.5,0.);
}
fn add(i:u32,x:f32){
    let z=x*1048576.;
    if(!fin(z)||z<=-2147483648.||z>=2147483648.){
        atomicOr(&s[0],2u);
        return;
    }let q=abs(z);
    let r=floor(q)+select(0.,1.,q-floor(q)>=.5);
    let n=i32(select(r,-r,z<0.));
    let old=atomicAdd(&a[i],n);
    if((n>0&&old>2147483647-n)||(n<0&&old<(-2147483647-1)-n)){
        atomicOr(&s[0],2u);
    }
}
@compute@workgroup_size(64)fn scatter(@builtin(global_invocation_id)x:vec3<u32>){
    if(x.x>=u.active_count){
        return;
    }let z=p[x.x];
    if(z.pm.w==0.){
        return;
    }if(!fin(z.pm.x)||!fin(z.pm.y)||!fin(z.pm.z)||!fin(z.pm.w)||!fin(z.vd.x)||!fin(z.vd.y)||!fin(z.vd.z)||!fin(z.cx.x)||!fin(z.cx.y)||!fin(z.cx.z)||!fin(z.cy.x)||!fin(z.cy.y)||!fin(z.cy.z)||!fin(z.cz.x)||!fin(z.cz.y)||!fin(z.cz.z)){
        atomicOr(&s[0],1u);
        return;
    }let o=vec3(u.ox,u.oy,u.oz);
    let r=array<vec4<f32>,3>(z.cx,z.cy,z.cz);
    for(var d=0u;
    d<3u;
    d++){
        let q=(z.pm.xyz-o)/u.h-face_offset(d);
        let b=vec3<i32>(floor(q-vec3(.5)));
        if(any(b<vec3<i32>(0))||any(b+vec3<i32>(2)>=vec3<i32>(i32(u.n)))){
            atomicOr(&s[0],4u);
            return;
        }let wx=ws(q.x-f32(b.x));
        let wy=ws(q.y-f32(b.y));
        let wz=ws(q.z-f32(b.z));
        for(var k=0u;
        k<3u;
        k++){
            for(var j=0u;
            j<3u;
            j++){
                for(var i=0u;
                i<3u;
                i++){
                    let c=b+vec3<i32>(i32(i),i32(j),i32(k));
                    let w=wx[i]*wy[j]*wz[k];
                    let fp=o+u.h*(vec3<f32>(c)+face_offset(d));
                    add(ix(c)*6u+d*2u,z.pm.w*w);
                    add(ix(c)*6u+d*2u+1u,z.pm.w*w*(z.vd[d]+dot(r[d].xyz,fp-z.pm.xyz)));
                }
            }
        }
    }
}
@compute@workgroup_size(64)fn resolve(@builtin(global_invocation_id)x:vec3<u32>){
    if(x.x>=u.n*u.n*u.n){
        return;
    }var q=vec4(0.);
    for(var d=0u;
    d<3u;
    d++){
        let m=f32(atomicLoad(&a[x.x*6u+d*2u]))/1048576.;
        let n=f32(atomicLoad(&a[x.x*6u+d*2u+1u]))/1048576.;
        if(m>0.){
            q[d]=n/m;
        }if(d==0u){
            q.w=m;
        }
    }g[x.x]=q;
}
@compute@workgroup_size(64)fn gather(@builtin(global_invocation_id)x:vec3<u32>){
    if(x.x>=u.active_count){
        return;
    }let z=p[x.x];
    if(z.pm.w==0.){
        return;
    }let o=vec3(u.ox,u.oy,u.oz);
    var v=vec3(0.);
    var c=array<vec3<f32>,3>(vec3(0.),vec3(0.),vec3(0.));
    for(var d=0u;
    d<3u;
    d++){
        let q=(z.pm.xyz-o)/u.h-face_offset(d);
        let b=vec3<i32>(floor(q-vec3(.5)));
        if(any(b<vec3<i32>(0))||any(b+vec3<i32>(2)>=vec3<i32>(i32(u.n)))){
            atomicOr(&s[0],4u);
            return;
        }let wx=ws(q.x-f32(b.x));
        let wy=ws(q.y-f32(b.y));
        let wz=ws(q.z-f32(b.z));
        for(var k=0u;
        k<3u;
        k++){
            for(var j=0u;
            j<3u;
            j++){
                for(var i=0u;
                i<3u;
                i++){
                    let cell=b+vec3<i32>(i32(i),i32(j),i32(k));
                    let w=wx[i]*wy[j]*wz[k];
                    let fp=o+u.h*(vec3<f32>(cell)+face_offset(d));
                    let va=g[ix(cell)][d];
                    v[d]+=w*va;
                    c[d]+=4./(u.h*u.h)*w*va*(fp-z.pm.xyz);
                }
            }
        }
    }var out=z;
    out.vd=vec4<f32>(v,z.vd.w);
    out.cx=vec4<f32>(c[0],z.cx.w);
    out.cy=vec4<f32>(c[1],z.cy.w);
    out.cz=vec4<f32>(c[2],z.cz.w);
    p[x.x]=out;
}
