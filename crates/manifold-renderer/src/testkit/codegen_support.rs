use crate::node_graph::freeze::codegen::ENTRY;
use crate::render_target::RenderTarget;
use half::f16;
use manifold_gpu::{
    GpuBinding, GpuDevice, GpuSamplerDesc, GpuTexture, GpuTextureDesc, GpuTextureDimension,
    GpuTextureFormat, GpuTextureUsage,
};

const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

pub(crate) fn gradient(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
    let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            px[i] = f16::from_f32(x as f32 / w as f32);
            px[i + 1] = f16::from_f32(y as f32 / h as f32);
            px[i + 2] = f16::from_f32(0.5);
            px[i + 3] = f16::from_f32(1.0);
        }
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: FMT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label: "codegen-input",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    tex
}

/// Dispatch a coincident two-input kernel: uniform(0), a(1), b(2),
/// sampler(3), dst(4). `param_bytes` is the 16-byte uniform payload.
pub(crate) fn dispatch_coincident(
    device: &GpuDevice,
    wgsl: &str,
    a: &GpuTexture,
    b: &GpuTexture,
    param_bytes: &[u8],
) -> RenderTarget {
    let (w, h) = (a.width, a.height);
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-test-mix");
    let sampler = device.create_sampler(&GpuSamplerDesc::default());
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out-mix");
    let mut enc = device.create_encoder("codegen-test-mix");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: param_bytes },
            GpuBinding::Texture { binding: 1, texture: a },
            GpuBinding::Texture { binding: 2, texture: b },
            GpuBinding::Sampler { binding: 3, sampler: &sampler },
            GpuBinding::Texture { binding: 4, texture: &out.texture },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-test-mix",
    );
    enc.commit_and_wait_completed();
    out
}

/// A second gradient with a different layout, so a + b differ per texel
/// (so the blend + crossfade is actually exercised).
pub(crate) fn gradient_b(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
    let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            px[i] = f16::from_f32(0.8 - 0.6 * (x as f32 / w as f32));
            px[i + 1] = f16::from_f32(0.2);
            px[i + 2] = f16::from_f32(y as f32 / h as f32);
            px[i + 3] = f16::from_f32(0.5);
        }
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: FMT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label: "codegen-input-b",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    tex
}

/// Dispatch a standard pointwise kernel: uniform(0), src(1), sampler(2),
/// dst(3). `param_bytes` is the 16-byte uniform payload.
pub(crate) fn dispatch_pointwise(
    device: &GpuDevice,
    wgsl: &str,
    input: &GpuTexture,
    param_bytes: &[u8],
) -> RenderTarget {
    let (w, h) = (input.width, input.height);
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-test");
    let sampler = device.create_sampler(&GpuSamplerDesc::default());
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out");
    let mut enc = device.create_encoder("codegen-test");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: param_bytes },
            GpuBinding::Texture { binding: 1, texture: input },
            GpuBinding::Sampler { binding: 2, sampler: &sampler },
            GpuBinding::Texture { binding: 3, texture: &out.texture },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-test",
    );
    enc.commit_and_wait_completed();
    out
}

/// Pack f32 params into a 16-byte-multiple uniform payload.
pub(crate) fn pack_f32(params: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for p in params {
        bytes.extend_from_slice(&p.to_le_bytes());
    }
    while bytes.len() % 16 != 0 {
        bytes.push(0);
    }
    bytes
}

/// Dispatch a two-input EXACT-TEXEL kernel: uniform(0), a(1), b(2), dst(3) —
/// NO sampler (both inputs are textureLoad'd). Mirrors dither's binding set.
pub(crate) fn dispatch_two_texel(
    device: &GpuDevice,
    wgsl: &str,
    a: &GpuTexture,
    b: &GpuTexture,
    param_bytes: &[u8],
) -> RenderTarget {
    let (w, h) = (a.width, a.height);
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-test-dither");
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out-dither");
    let mut enc = device.create_encoder("codegen-test-dither");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: param_bytes },
            GpuBinding::Texture { binding: 1, texture: a },
            GpuBinding::Texture { binding: 2, texture: b },
            GpuBinding::Texture { binding: 3, texture: &out.texture },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-test-dither",
    );
    enc.commit_and_wait_completed();
    out
}

/// Dispatch an N-input coincident kernel: uniform(0), inputs(1..=N),
/// sampler(N+1), dst(N+2) — the generated MultiInputCoincident layout for any
/// arity. Generalizes `dispatch_coincident` (which is fixed at 2 inputs).
pub(crate) fn dispatch_coincident_n(
    device: &GpuDevice,
    wgsl: &str,
    inputs: &[&GpuTexture],
    param_bytes: &[u8],
) -> RenderTarget {
    let (w, h) = (inputs[0].width, inputs[0].height);
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-coincident-n");
    let sampler = device.create_sampler(&GpuSamplerDesc::default());
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out-coincident-n");
    let mut bindings: Vec<GpuBinding> =
        vec![GpuBinding::Bytes { binding: 0, data: param_bytes }];
    for (i, t) in inputs.iter().enumerate() {
        bindings.push(GpuBinding::Texture { binding: (i + 1) as u32, texture: t });
    }
    bindings.push(GpuBinding::Sampler {
        binding: (inputs.len() + 1) as u32,
        sampler: &sampler,
    });
    bindings.push(GpuBinding::Texture {
        binding: (inputs.len() + 2) as u32,
        texture: &out.texture,
    });
    let mut enc = device.create_encoder("codegen-coincident-n");
    enc.dispatch_compute(
        &pipeline,
        &bindings,
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-coincident-n",
    );
    enc.commit_and_wait_completed();
    out
}

/// Dispatch a PARAMLESS pointwise kernel: tex(0), sampler(1), dst(2) — no
/// uniform binding (a paramless atom's generated kernel binds none).
pub(crate) fn dispatch_paramless_pointwise(
    device: &GpuDevice,
    wgsl: &str,
    input: &GpuTexture,
) -> RenderTarget {
    let (w, h) = (input.width, input.height);
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-paramless");
    let sampler = device.create_sampler(&GpuSamplerDesc::default());
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out-paramless");
    let mut enc = device.create_encoder("codegen-paramless");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Texture { binding: 0, texture: input },
            GpuBinding::Sampler { binding: 1, sampler: &sampler },
            GpuBinding::Texture { binding: 2, texture: &out.texture },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-paramless",
    );
    enc.commit_and_wait_completed();
    out
}

/// Allocate an n×n×n 3D texture with the given usage.
pub(crate) fn make_3d_texture(device: &GpuDevice, n: u32, usage: GpuTextureUsage, label: &'static str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: n,
        height: n,
        depth: n,
        format: FMT,
        dimension: GpuTextureDimension::D3,
        usage,
        label,
        mip_levels: 1,
    })
}

/// Fill an n³ density volume on-GPU with a 3D gradient (varies along x/y/z).
pub(crate) fn fill_volume_gradient(device: &GpuDevice, vol: &GpuTexture, n: u32) {
    let fill_wgsl = "\
@group(0) @binding(0) var vol: texture_storage_3d<rgba16float, write>;\n\
@compute @workgroup_size(4, 4, 4)\n\
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {\n\
let d = textureDimensions(vol);\n\
if id.x >= d.x || id.y >= d.y || id.z >= d.z { return; }\n\
let f = vec3<f32>(id) / vec3<f32>(d);\n\
textureStore(vol, vec3<i32>(id), vec4<f32>(f.x, f.y, f.z, 0.5 + 0.5 * f.x));\n\
}\n";
    let pipeline = device.create_compute_pipeline(fill_wgsl, ENTRY, "vol-fill");
    let mut enc = device.create_encoder("vol-fill");
    enc.dispatch_compute(
        &pipeline,
        &[GpuBinding::Texture { binding: 0, texture: vol }],
        [n.div_ceil(4), n.div_ceil(4), n.div_ceil(4)],
        "vol-fill",
    );
    enc.commit_and_wait_completed();
}

/// Read back a full n³ volume as f16 bits.
pub(crate) fn readback_volume(device: &GpuDevice, vol: &GpuTexture, n: u32) -> Vec<u16> {
    let bytes_per_row = n * 8; // rgba16float
    let total = u64::from(bytes_per_row) * u64::from(n) * u64::from(n);
    let buf = device.create_buffer_shared(total);
    let mut renc = device.create_encoder("vol-readback");
    renc.copy_texture_3d_to_buffer(vol, &buf, n, n, n, bytes_per_row);
    renc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared buffer pointer");
    let halves: &[u16] =
        unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (n * n * n * 4) as usize) };
    halves.to_vec()
}

/// Dispatch a two-output SOURCE kernel: uniform(0), dst_a(1), dst_b(2). Both
/// outputs get their own texture (no aliasing) so each can be diffed.
pub(crate) fn dispatch_two_output_source(
    device: &GpuDevice,
    wgsl: &str,
    param_bytes: &[u8],
    w: u32,
    h: u32,
) -> (RenderTarget, RenderTarget) {
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-multi-out");
    let a = RenderTarget::new(device, w, h, FMT, "codegen-out-a");
    let b = RenderTarget::new(device, w, h, FMT, "codegen-out-b");
    let mut enc = device.create_encoder("codegen-multi-out");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes { binding: 0, data: param_bytes },
            GpuBinding::Texture { binding: 1, texture: &a.texture },
            GpuBinding::Texture { binding: 2, texture: &b.texture },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-multi-out",
    );
    enc.commit_and_wait_completed();
    (a, b)
}

/// Dispatch a SOURCE (generator) kernel: [uniform(0)], output. No texture
/// inputs, no sampler — a paramless source binds only its output at binding 0.
pub(crate) fn dispatch_source(
    device: &GpuDevice,
    wgsl: &str,
    param_bytes: Option<&[u8]>,
    w: u32,
    h: u32,
) -> RenderTarget {
    let pipeline = device.create_compute_pipeline(wgsl, ENTRY, "codegen-source");
    let out = RenderTarget::new(device, w, h, FMT, "codegen-out-source");
    let mut bindings: Vec<GpuBinding> = Vec::new();
    let mut next = 0u32;
    if let Some(bytes) = param_bytes {
        bindings.push(GpuBinding::Bytes { binding: 0, data: bytes });
        next = 1;
    }
    bindings.push(GpuBinding::Texture { binding: next, texture: &out.texture });
    let mut enc = device.create_encoder("codegen-source");
    enc.dispatch_compute(
        &pipeline,
        &bindings,
        [w.div_ceil(16), h.div_ceil(16), 1],
        "codegen-source",
    );
    enc.commit_and_wait_completed();
    out
}
