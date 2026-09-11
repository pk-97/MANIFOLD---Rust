// One checkerboard sweep per graph node. Atomic scalar storage prevents
// alias races; nearest neighbors always have the opposite parity.
struct Row {mac_lower_diag:vec4<f32>,mac_upper_rhs:vec4<f32>}
struct Params {parity:u32,omega:f32,absolute_tolerance:f32,relative_tolerance:f32}
@group(0) @binding(0) var<uniform> params:Params;
@group(0) @binding(1) var<storage,read> rows:array<Row>;
@group(0) @binding(2) var<storage,read_write> pressure:array<atomic<u32>>;
@group(0) @binding(3) var<storage,read_write> status:array<atomic<u32>>;
fn neighbor_sum(i:u32,r:Row)->f32{
    let strides=array<u32,3>(1u,64u,4096u);var sum=0.0;
    for(var a=0u;a<3u;a++){
        if(r.mac_lower_diag[a]>0.0){sum+=r.mac_lower_diag[a]*bitcast<f32>(atomicLoad(&pressure[i-strides[a]]));}
        if(r.mac_upper_rhs[a]>0.0){sum+=r.mac_upper_rhs[a]*bitcast<f32>(atomicLoad(&pressure[i+strides[a]]));}
    }
    return sum;
}
@compute @workgroup_size(256)
fn cs_relax(@builtin(global_invocation_id) gid:vec3<u32>){
    let i=gid.x;if(i>=262144u){return;}
    let c=mac_cell_coord(i);if(u32(c.x+c.y+c.z)%2u!=params.parity){return;}
    let row=rows[i];let diag=row.mac_lower_diag.w;
    if(diag<=0.0){atomicStore(&pressure[i],0u);return;}
    let old=bitcast<f32>(atomicLoad(&pressure[i]));
    let next=(row.mac_upper_rhs.w+neighbor_sum(i,row))/diag;
    atomicStore(&pressure[i],bitcast<u32>(old+params.omega*(next-old)));
}
@compute @workgroup_size(256)
fn cs_validate(@builtin(global_invocation_id) gid:vec3<u32>){
    let i=gid.x;if(i>=262144u){return;}
    let row=rows[i];let diag=row.mac_lower_diag.w;
    let q=bitcast<f32>(atomicLoad(&pressure[i]));
    let residual=diag*q-neighbor_sum(i,row)-row.mac_upper_rhs.w;
    let bound=params.absolute_tolerance*WATER_H*WATER_H+params.relative_tolerance*abs(row.mac_upper_rhs.w);
    if(!water_finite1(diag)||!water_finite1(q)||!water_finite1(residual)||abs(residual)>bound){atomicOr(&status[0],32u);}
}
