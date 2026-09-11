// Scalar field level set, evaluated independently per screen pixel. The
// optical path sums liquid intervals and subtracts the exact solid interval.
fn iso_box(ro: vec3<f32>, rd: vec3<f32>, lo: vec3<f32>, hi: vec3<f32>) -> vec2<f32> {
    var a = -1e20; var b = 1e20;
    for (var k=0u; k<3u; k++) {
        if (abs(rd[k]) < 1e-8) {
            if (ro[k] < lo[k] || ro[k] > hi[k]) { return vec2<f32>(1.0,-1.0); }
        } else {
            let x=(lo[k]-ro[k])/rd[k]; let y=(hi[k]-ro[k])/rd[k];
            a=max(a,min(x,y)); b=min(b,max(x,y));
        }
    }
    return vec2<f32>(a,b);
}
fn iso_sample(tex: texture_3d<f32>, smp: sampler, p: vec3<f32>) -> vec4<f32> {
    return textureSampleLevel(tex,smp,(p-vec3<f32>(-2.0,0.0,-2.0))*0.25,0.0);
}
// Returns earliest unoccluded liquid and its total length for this span.
fn iso_span(a:f32,b:f32,solid:vec2<f32>) -> vec2<f32> {
    if(b<=a){return vec2<f32>(-1.0,0.0);}
    let overlap=max(0.0,min(b,solid.y)-max(a,solid.x));
    let length=max(0.0,b-a-overlap);
    if(length<=1e-7){return vec2<f32>(-1.0,0.0);}
    var first=a;
    if(a>=solid.x && a<solid.y){first=solid.y;}
    return vec2<f32>(first,length);
}
fn body(density: texture_3d<f32>, smp: sampler, uv:vec2<f32>, dims:vec2<f32>,
    isovalue:f32, step_scale:f32, cube_half_x:f32,cube_half_y:f32,cube_half_z:f32,
    cam_pos:vec3<f32>,cam_fwd:vec3<f32>,cam_right:vec3<f32>,cam_up:vec3<f32>,
    fov_y:f32,near:f32,far:f32,collider_center:vec3<f32>,collider_enabled:u32) -> BodyOutputs {
    let empty=BodyOutputs(vec4<f32>(1.0,0.0,0.0,0.0),vec4<f32>(0.0),vec4<f32>(0.0),vec4<f32>(0.0),vec4<f32>(0.0));
    let tan_fov=tan(fov_y*0.5);
    let rd=normalize(cam_fwd+cam_right*((uv.x*2.0-1.0)*dims.x/dims.y*tan_fov)+cam_up*((1.0-uv.y*2.0)*tan_fov));
    let forward=dot(rd,cam_fwd);
    let bounds=iso_box(cam_pos,rd,vec3<f32>(-2.0,0.0,-2.0),vec3<f32>(2.0,4.0,2.0));
    let begin=max(bounds.x,near/forward); let end=min(bounds.y,far/forward);
    if(end<=begin || forward<=0.0){return empty;}
    var solid=vec2<f32>(1e20,1e20);
    let half=vec3<f32>(cube_half_x,cube_half_y,cube_half_z);
    if(collider_enabled!=0u){solid=iso_box(cam_pos,rd,collider_center-half,collider_center+half);}
    let voxel=4.0/vec3<f32>(textureDimensions(density));
    let step=min(voxel.x,min(voxel.y,voxel.z))*clamp(step_scale,0.5,1.0);
    var t=begin;
    var prev=iso_sample(density,smp,cam_pos+rd*t).r-isovalue;
    var span_start=select(-1.0,t,prev>=0.0);
    var first=-1.0; var thickness=0.0;
    // Even 512^3 at half-voxel spacing fits within 1774 diagonal samples.
    for(var i=0u;i<4096u;i++){
        let tn=min(t+step,end);
        let current=iso_sample(density,smp,cam_pos+rd*tn).r-isovalue;
        let was_inside=prev>=0.0; let inside=current>=0.0;
        if(was_inside!=inside){
            var a=t; var b=tn;
            for(var j=0u;j<5u;j++){
                let mid=(a+b)*0.5;
                let mid_inside=iso_sample(density,smp,cam_pos+rd*mid).r>=isovalue;
                if(mid_inside==was_inside){a=mid;}else{b=mid;}
            }
            let cross=(a+b)*0.5;
            if(inside){span_start=cross;}else{
                let span=iso_span(span_start,cross,solid);
                if(first<0.0 && span.y>0.0){first=span.x;}
                thickness+=span.y; span_start=-1.0;
            }
        }
        t=tn; prev=current;
        if(t>=end){break;}
    }
    if(span_start>=0.0){
        let span=iso_span(span_start,end,solid);
        if(first<0.0 && span.y>0.0){first=span.x;}
        thickness+=span.y;
    }
    if(first<0.0 || thickness<=0.0){return empty;}
    let p=cam_pos+rd*first;
    let gx=(iso_sample(density,smp,p+vec3<f32>(voxel.x,0.0,0.0)).r-iso_sample(density,smp,p-vec3<f32>(voxel.x,0.0,0.0)).r)/voxel.x;
    let gy=(iso_sample(density,smp,p+vec3<f32>(0.0,voxel.y,0.0)).r-iso_sample(density,smp,p-vec3<f32>(0.0,voxel.y,0.0)).r)/voxel.y;
    let gz=(iso_sample(density,smp,p+vec3<f32>(0.0,0.0,voxel.z)).r-iso_sample(density,smp,p-vec3<f32>(0.0,0.0,voxel.z)).r)/voxel.z;
    var n=-vec3<f32>(gx,gy,gz);
    if(collider_enabled!=0u && abs(first-solid.y)<step/32.0){
        let q=p-collider_center; let d=abs(abs(q)-half);
        if(d.x<=d.y && d.x<=d.z){n=vec3<f32>(-sign(q.x),0.0,0.0);}
        else if(d.y<=d.z){n=vec3<f32>(0.0,-sign(q.y),0.0);}
        else{n=vec3<f32>(0.0,0.0,-sign(q.z));}
    }
    if(dot(n,n)<1e-12){n=-rd;}else{n=normalize(n);}
    let view_normal=vec3<f32>(dot(n,cam_right),dot(n,cam_up),dot(n,cam_fwd));
    let vz=first*forward;
    let raw=clamp(far/(near-far)*(near/vz-1.0),0.0,0.99999994);
    let foam=clamp(iso_sample(density,smp,p).g,0.0,1.0);
    return BodyOutputs(vec4<f32>(raw,0.0,0.0,1.0),vec4<f32>(thickness,0.0,0.0,1.0),vec4<f32>(view_normal,1.0),vec4<f32>(1.0,0.0,0.0,1.0),vec4<f32>(foam,0.0,0.0,1.0));
}
