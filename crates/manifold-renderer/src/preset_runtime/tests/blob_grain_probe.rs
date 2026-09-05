    //! Regression: master chain [BlobTracking, FilmGrain] with zero blobs
    //! detected must pass the video through. Pre-fix, a skip-passthrough
    //! alias borrow leaked across shared physical slots and FilmGrain's
    //! noise dispatch wrote into the borrowed upstream texture — the
    //! output was grain over grain (the input texture itself could be
    //! clobbered). Fix: `MetalBackend::acquire` drops executor-installed
    //! skip-alias borrows when a slot changes tenant.
    //!
    //! Solid red is the oracle: overlay-mode grain over pure red is
    //! red-invariant, so a correct chain outputs red; a chain whose mix
    //! reads the noise texture outputs gray noise.
    //!
    //! Covered by the default gpu-proofs suite — no --ignored flag.

    use super::*;
    use crate::gpu_encoder::GpuEncoder;
    use crate::preset_context::PresetContext;
    use half::f16;
    use manifold_core::PresetTypeId;
    use manifold_core::effects::PresetInstance;
    use manifold_gpu::{
        GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    fn set_param(fx: &mut PresetInstance, id: &str, v: f32) {
        let ty = fx.effect_type().clone();
        let p = fx
            .params
            .get_mut(id)
            .unwrap_or_else(|| panic!("param id `{id}` on {ty:?}"));
        p.value = v;
        p.base = v;
    }

    fn ctx(w: u32, h: u32, frame: i64) -> PresetContext {
        PresetContext {
            time: frame as f64 / 60.0,
            beat: frame as f64 / 30.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: w as f32 / h as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
            gpu_signal_committed: 0,
            gpu_signaled: 0,
        }
    }

    fn solid_input(device: &manifold_gpu::GpuDevice, w: u32, h: u32) -> manifold_gpu::GpuTexture {
        let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
        for p in px.chunks_exact_mut(4) {
            p[0] = f16::from_f32(1.0);
            p[1] = f16::from_f32(0.0);
            p[2] = f16::from_f32(0.0);
            p[3] = f16::from_f32(1.0);
        }
        let tex = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "blob-grain-probe-input",
            mip_levels: 1,
        });
        let bytes = unsafe {
            std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
        };
        device.upload_texture(&tex, bytes);
        tex
    }

    /// Per-channel (mean, stddev) over a raw Rgba16Float readback.
    fn channel_stats(raw: &[u8]) -> ([f32; 4], [f32; 4]) {
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(raw.as_ptr().cast::<u16>(), raw.len() / 2) };
        let n = halves.len() / 4;
        let mut mean = [0f64; 4];
        let mut var = [0f64; 4];
        for px in halves.chunks_exact(4) {
            for c in 0..4 {
                let v = f16::from_bits(px[c]).to_f32() as f64;
                mean[c] += v;
                var[c] += v * v;
            }
        }
        let denom = n.max(1) as f64;
        let mut m = [0f32; 4];
        let mut s = [0f32; 4];
        for c in 0..4 {
            m[c] = (mean[c] / denom) as f32;
            s[c] = ((var[c] / denom - (mean[c] / denom) * (mean[c] / denom)).max(0.0)).sqrt() as f32;
        }
        (m, s)
    }

    fn assert_solid_red(device: &manifold_gpu::GpuDevice, tex: &manifold_gpu::GpuTexture, w: u32, h: u32, what: &str) {
        let raw = crate::headless_readback::readback_raw_halves(device, tex, w, h);
        let (m, s) = channel_stats(&raw);
        assert!(
            (m[0] - 1.0).abs() < 0.02
                && m[1].abs() < 0.02
                && m[2].abs() < 0.02
                && s[0] < 0.05
                && s[1] < 0.02
                && s[2] < 0.02,
            "{what} must be solid red, got mean={m:?} std={s:?}"
        );
    }

    #[test]
    fn blob_grain_empty_detections_passes_video_through() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let (w, h) = (256u32, 256u32);
        let input = solid_input(&device, w, h);

        let mut blob = make_default(PresetTypeId::BLOB_TRACKING);
        set_param(&mut blob, "threshold", 0.9); // near-max → zero blobs
        let grain = make_default(PresetTypeId::new("FilmGrain"));
        let effects = vec![blob, grain];

        let mut cg = PresetRuntime::try_build(
            ChainBuildInputs {
                effects: &effects,
                groups: &[],
                primitives: &primitives,
                device: &device,
                pool: None,
                width: w,
                height: h,
                preview_effect: None,
            },
            None,
        )
        .expect("chain builds");

        let run_frame = |cg: &mut PresetRuntime, frame: i64| {
            let mut enc = device.create_encoder("blob-grain-probe");
            {
                let mut gpu = GpuEncoder::new(&mut enc, &device);
                cg.run(&mut gpu, &input, &effects, &[], &ctx(w, h, frame));
            }
            enc.commit_and_wait_completed();
        };

        // Warm up to steady state (blob detection worker + two-frame empty guard).
        for f in 0..60 {
            run_frame(&mut cg, f);
        }
        run_frame(&mut cg, 60);

        // The chain must not write into the host's input texture, and its
        // output must be the passthrough video (red-invariant under overlay
        // grain), not grain over grain.
        assert_solid_red(&device, &input, w, h, "chain input texture");
        let output = cg.output_texture().expect("chain output").clone();
        assert_solid_red(&device, &output, w, h, "chain output");
    }
