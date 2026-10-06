//! CPU shader validation and fixtures shared with GPU sheeting proofs.

/// Restore the pre-stage-1 selection and both per-site merges. The surrounding
/// indexing, arithmetic, draw and write stay shared; no production switch exists.
pub(super) fn reference_shader() -> String {
    let source = include_str!("shaders/gpu_flip_sheeting.wgsl");
    let start = source.find("// Phase 2:").unwrap();
    let end = source.find("fn bucket_dims()").unwrap();
    let mut source = format!(
        "{}{}\n@compute @workgroup_size(256)\nfn build_buckets() {{}}\n{}",
        &source[..start], OLD_SELECTION, &source[end..],
    );
    let row_read = "let row = bucket_flat(nb);\n                let n = sheet_b[row];";
    assert_eq!(source.matches(row_read).count(), 2);
    assert_eq!(source.matches("let np = selected[32u * row + m].xyz;").count(), 2);
    source = source.replace(row_read, "let n = fill_bucket(nb);")
        .replace("let np = selected[32u * row + m].xyz;", "let np = bucket[m].xyz;")
        .replace("cell_counts", "selected_count");
    source
}

/// CPU-only: validate both complete WGSL modules, including the test reference.
#[test]
fn sheeting_bucket_shaders_validate_on_cpu() {
    for source in [include_str!("shaders/gpu_flip_sheeting.wgsl").to_owned(), reference_shader()] {
        let module = naga::front::wgsl::parse_str(&source).expect("WGSL parse");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module).expect("WGSL validation");
    }
}

pub(super) struct BucketFixture {
    pub(super) counts: Vec<u32>,
    pub(super) segments: Vec<[u32; 4]>,
    pub(super) merged: Vec<Vec<[u32; 4]>>,
}

/// Shuffled input indices are distributed to cells, then stably sorted per
/// cell as ParticleSorter does. CPU reference uses a stable sort of each
/// bucket's concatenated lists, independent of the shader's eight-way merge.
pub(super) fn bucket_fixture(cells: [usize; 3], saturated: bool) -> BucketFixture {
    let buckets = cells.map(|n| n.div_ceil(2));
    let n = cells.iter().product::<usize>();
    let rows = buckets.iter().product::<usize>();
    let mut indices: Vec<u32> = (0..4 * n as u32).collect();
    let mut rng = 0x1234_5678u32;
    for i in (1..indices.len()).rev() {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        indices.swap(i, rng as usize % (i + 1));
    }
    let mut fixture = BucketFixture {
        counts: vec![0; n], segments: vec![[0xdead_beef; 4]; 32 * rows], merged: vec![Vec::new(); rows],
    };
    for c in 0..n {
        let q = [c % cells[0], c / cells[0] % cells[1], c / (cells[0] * cells[1])];
        let b = q.map(|v| v / 2);
        let row = b[0] + buckets[0] * (b[1] + buckets[1] * b[2]);
        let lane = (q[0] & 1) + 2 * (q[1] & 1) + 4 * (q[2] & 1);
        let count = if saturated { 4 } else { c % 5 };
        fixture.counts[c] = count as u32;
        let list = &mut indices[4 * c..4 * c + count];
        list.sort_unstable();
        for (m, &index) in list.iter().enumerate() {
            // High index bits include NaN encodings: .w is never numeric data.
            let marker = [(c as f32).to_bits(), (m as f32).to_bits(), (-0.0f32).to_bits(), 0x7fc0_0000 + index];
            fixture.segments[32 * row + 4 * lane + m] = marker;
            fixture.merged[row].push(marker);
        }
    }
    for row in &mut fixture.merged {
        row.sort_by_key(|p| p[3]);
    }
    fixture
}

#[test]
fn sheeting_bucket_fixture_covers_padding_and_saturation() {
    let fixture = bucket_fixture([5, 3, 7], true);
    assert_eq!(fixture.merged.iter().map(Vec::len).sum::<usize>(), 4 * 5 * 3 * 7);
    assert_eq!(fixture.counts.iter().sum::<u32>(), 4 * 5 * 3 * 7);
    assert!(fixture.merged.iter().any(|row| row.len() == 32));
    assert_eq!(fixture.merged.last().unwrap().len(), 4);
    assert!(fixture.merged.iter().all(|row| row.windows(2).all(|w| w[0][3] < w[1][3])));
    assert_ne!(&fixture.segments[..32], fixture.merged[0].as_slice(), "merge must reorder unread segments");
    let empty = bucket_fixture([1, 1, 1], false);
    assert!(empty.merged[0].is_empty());
}

// Frozen selection/merge from 09813ff8f; test builds only.
const OLD_SELECTION: &str = r#"// Phase 2: the first four markers of each sheet cell, in input order, with
// -2h <= phi < 2h.
@compute @workgroup_size(256)
fn select_markers(@builtin(global_invocation_id) gid: vec3<u32>) {
    if !clock_active() { return; }
    let c = gid.x;
    if c >= u.nx * u.ny * u.nz || atomicLoad(&sheet_a[c]) == 0u { return; }
    let range = ranges[c];
    var n = 0u;
    for (var s = range.start; s < range.start + range.count && n < MAX_SHEET_PARTICLES_PER_CELL; s = s + 1u) {
        let p = local(particles[s]);
        let value = sample(p);
        if value >= u.max_depth || value < -u.max_depth { continue; }
        selected[4u * c + n] = vec4<f32>(p, bitcast<f32>(order[s]));
        n = n + 1u;
    }
    selected_count[c] = n;
}

// One coarse 2-cell bucket's phase-2 markers in input order: the eight
// cells' lists (each in input order) merged.
var<private> bucket: array<vec4<f32>, 32>;
fn fill_bucket(b: vec3<i32>) -> u32 {
    var heads: array<u32, 8>;
    var counts: array<u32, 8>;
    var cells: array<u32, 8>;
    for (var l = 0; l < 8; l = l + 1) {
        let c = 2 * b + vec3<i32>(l & 1, (l >> 1) & 1, (l >> 2) & 1);
        heads[l] = 0u;
        counts[l] = 0u;
        if in_range(c) {
            cells[l] = flat(c);
            counts[l] = selected_count[cells[l]];
        }
    }
    var n = 0u;
    loop {
        var best = -1;
        var best_index = 0xffffffffu;
        for (var l = 0; l < 8; l = l + 1) {
            if heads[l] < counts[l] {
                let index = bitcast<u32>(selected[4u * cells[l] + heads[l]].w);
                if index < best_index { best_index = index; best = l; }
            }
        }
        if best < 0 { break; }
        bucket[n] = selected[4u * cells[best] + heads[best]];
        heads[best] = heads[best] + 1u;
        n = n + 1u;
    }
    return n;
}

"#;
