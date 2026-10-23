//! Sealed pockets: the spread, its replay gate and cap, and the pocket mean at a coarse solve level.

use super::*;

/// The engine's sealed pockets on CPU (PressureSolver::_conditionSolidVelocityField):
/// water cells link through a face open at least 1e-6; a region reaches air
/// through a linked dry neighbour or an open tank face (`mask`); a cell of an
/// unreached region of more than one cell is isolated. Returns the isolated
/// cells, then every sealed (wet, unreached) cell.
fn cpu_isolated(water: &[f32], open: &[FaceSample], mask: u32) -> (Vec<bool>, Vec<bool>) {
    let wet = |c: [usize; 3]| water[cell_index(c)] > 0.5;
    let neighbours = |c: [usize; 3]| {
        let mut out = Vec::new();
        for a in 0..3 {
            for d in [c[a].checked_sub(1), Some(c[a] + 1).filter(|&q| q < N[a])].into_iter().flatten() {
                let mut q = c;
                q[a] = d;
                let mut f = c;
                f[a] = c[a].max(d);
                if open[pad_index(f)].weight[a] >= 1e-6 {
                    out.push(q);
                }
            }
        }
        out
    };
    let mut reach: Vec<bool> = (0..cell_len())
        .map(|i| {
            let c = cell_coords(i);
            let face = (0..3).any(|a| (mask >> (2 * a) & 1 == 0 && c[a] == 0) || (mask >> (2 * a + 1) & 1 == 0 && c[a] == N[a] - 1));
            wet(c) && (face || neighbours(c).iter().any(|&q| !wet(q)))
        })
        .collect();
    let mut queue: Vec<usize> = (0..cell_len()).filter(|&i| reach[i]).collect();
    while let Some(i) = queue.pop() {
        for q in neighbours(cell_coords(i)) {
            let j = cell_index(q);
            if wet(q) && !reach[j] {
                reach[j] = true;
                queue.push(j);
            }
        }
    }
    let isolated = (0..cell_len())
        .map(|i| {
            let c = cell_coords(i);
            wet(c) && !reach[i] && neighbours(c).iter().any(|&q| wet(q))
        })
        .collect();
    let sealed = (0..cell_len()).map(|i| wet(cell_coords(i)) && !reach[i]).collect();
    (isolated, sealed)
}

/// The sealed water's pockets on CPU: each cell of `sealed` labelled by the
/// lowest cell index it links to through sealed cells; other cells None.
fn cpu_pocket_labels(sealed: &[bool], open: &[FaceSample]) -> Vec<Option<usize>> {
    let mut label = vec![None; cell_len()];
    for start in 0..cell_len() {
        if !sealed[start] || label[start].is_some() {
            continue;
        }
        // Ascending start, so the first cell reached is the lowest index.
        label[start] = Some(start);
        let mut queue = vec![start];
        while let Some(i) = queue.pop() {
            let c = cell_coords(i);
            for a in 0..3 {
                for d in [c[a].checked_sub(1), Some(c[a] + 1).filter(|&q| q < N[a])].into_iter().flatten() {
                    let mut q = c;
                    q[a] = d;
                    let mut f = c;
                    f[a] = c[a].max(d);
                    let j = cell_index(q);
                    if sealed[j] && label[j].is_none() && open[pad_index(f)].weight[a] >= 1e-6 {
                        label[j] = Some(start);
                        queue.push(j);
                    }
                }
            }
        }
    }
    label
}

/// Runs the pocket passes over `water`, `open` and the solid velocity
/// `moving`, rounds until one changes nothing; returns the conditioned
/// velocity, the rounds that changed a cell, then `rhs` with each sealed
/// pocket's mean removed and the solver words after it.
fn gpu_pockets(water: &[f32], open: &[FaceSample], moving: &[FaceSample], mask: u32, rhs: &[f32]) -> (Vec<FaceSample>, usize, Vec<f32>, Vec<u32>) {
    let params = StepParams { closed_faces: mask, ..lattice() };
    let lines = (N[1] * N[2]).max(N[0] * N[2]).max(N[0] * N[1]);
    let mut pass = Pass::new();
    pass.bind(6, water)
        .bind(10, open)
        .bind(23, &vec![0u32; cell_len()])
        .bind(24, &[0u32; POCKET_GATE_WORDS as usize])
        .bind(4, moving)
        .bind(25, &vec![0u32; cell_len()])
        .bind(5, rhs)
        .bind(26, &vec![7u32; 3 * cell_len() + 2])
        .bind(22, &[9u32; 6]);
    pass.run::<u32>("pocket_seed", &params, 23, cell_len(), cell_len());
    pass.run::<u32>("pocket_start", &params, 24, POCKET_GATE_WORDS as usize, 1);
    let mut rounds = 0;
    loop {
        pass.run::<u32>("pocket_round", &params, 24, POCKET_GATE_WORDS as usize, 1);
        for sweep in ["pocket_sweep_x", "pocket_sweep_y", "pocket_sweep_z"] {
            pass.run::<u32>(sweep, &params, 24, POCKET_GATE_WORDS as usize, lines);
        }
        if pass.bound::<u32>(24, POCKET_GATE_WORDS as usize)[9] == 0 {
            break;
        }
        rounds += 1;
        assert!(rounds <= cell_len(), "the spread never settled");
    }
    pass.run::<u32>("pocket_check", &params, 24, POCKET_GATE_WORDS as usize, cell_len());
    assert_eq!(pass.bound::<u32>(24, POCKET_GATE_WORDS as usize)[10], 0, "a settled spread leaves no sealed cell linked to air");
    let got = pass.run::<FaceSample>("pocket_condition", &params, 4, face_len(), face_len());
    pass.run::<u32>("pocket_clear", &params, 26, 3 * cell_len() + 2, 3 * cell_len() + 2);
    pass.run::<u32>("pocket_accumulate", &params, 26, 3 * cell_len() + 2, cell_len());
    let removed = pass.run::<f32>("pocket_remove", &params, 5, cell_len(), cell_len());
    let words = pass.run::<u32>("pocket_flux_pressure", &StepParams { step_in_tick: 0, ..params }, 22, 6, 1);
    (got, rounds, removed, words)
}

#[test]
fn gpu_flip_sealed_pockets_zero_the_solid_velocity_as_the_engine() {
    let mut zeroed = 0;
    // A random solid seals nothing off, so half its faces are closed again
    // and nearly every cell is water: walls of closed faces split the water
    // into pockets, some reaching a dry cell or open face and some not.
    for (seed, mask) in [(0x5e1, 63), (0x5e2, 63), (0x5e3, 63 & !(1 << 3)), (0x5e4, 63 & !1)] {
        let mut rng = Stream::new(seed);
        let water: Vec<f32> = (0..cell_len()).map(|_| f32::from(u8::from(rng.unit() < 0.97))).collect();
        let mut open = solid_faces(seed + 1, true);
        for face in &mut open {
            for a in 0..3 {
                if rng.unit() < 0.5 {
                    face.weight[a] = 0.0;
                }
            }
        }
        let moving: Vec<FaceSample> = (0..face_len())
            .map(|i| {
                let p = pad_coords(i);
                let mut face = FaceSample::default();
                for a in 0..3 {
                    if face_exists(p, a) {
                        face.velocity[a] = 0.5 + rng.unit();
                    }
                }
                face.velocity[3] = 7.0;
                face
            })
            .collect();
        let (isolated, sealed) = cpu_isolated(&water, &open, mask);
        let rhs: Vec<f32> = (0..cell_len()).map(|_| 4.0 * rng.unit() - 1.0).collect();
        let (got, rounds, removed, words) = gpu_pockets(&water, &open, &moving, mask, &rhs);
        // Each sealed pocket loses its mean, so it sums to 0; every other
        // cell keeps its value. The GPU rounds each value to 1/65536 first.
        let labels = cpu_pocket_labels(&sealed, &open);
        let mut sums = std::collections::BTreeMap::<usize, (f64, usize)>::new();
        for (i, label) in labels.iter().enumerate() {
            if let Some(l) = label {
                let entry = sums.entry(*l).or_default();
                entry.0 += f64::from(rhs[i]);
                entry.1 += 1;
            }
        }
        let mut total = 0.0;
        for (i, label) in labels.iter().enumerate() {
            let want = match label {
                Some(l) => {
                    let (sum, count) = sums[l];
                    if i == *l {
                        total += sum.abs();
                    }
                    f64::from(rhs[i]) - sum / count as f64
                }
                None => f64::from(rhs[i]),
            };
            let tolerance = if label.is_some() { 1e-4 } else { 0.0 };
            assert!((f64::from(removed[i]) - want).abs() <= tolerance, "seed {seed:#x} mask {mask}: cell {i} rhs {} want {want}", removed[i]);
        }
        for l in sums.keys() {
            let residual: f64 = (0..cell_len()).filter(|&i| labels[i] == Some(*l)).map(|i| f64::from(removed[i])).sum();
            assert!(residual.abs() < 1e-3, "seed {seed:#x} mask {mask}: pocket {l} sums to {residual} after its mean is removed");
        }
        let h = f64::from(lattice().cell_size);
        let flux = f64::from(f32::from_bits(words[4]));
        assert!((flux - total * h * h * h).abs() <= 1e-4 * (total * h * h * h).max(1e-9), "seed {seed:#x} mask {mask}: removed flux {flux}, want {}", total * h * h * h);
        assert_eq!(words[5], 0, "the tick's first step clears the density word");
        assert_eq!(&words[..4], &[9, 9, 9, 9], "the other solver words stay");
        for i in 0..face_len() {
            let p = pad_coords(i);
            for a in 0..3 {
                if !face_exists(p, a) {
                    continue;
                }
                let mut below = p;
                let low = p[a] > 0 && {
                    below[a] = p[a] - 1;
                    isolated[cell_index(below)]
                };
                let high = p[a] < N[a] && isolated[cell_index(p)];
                let want = if low || high { 0.0 } else { moving[i].velocity[a] };
                zeroed += usize::from(low || high);
                assert_eq!(got[i].velocity[a], want, "seed {seed:#x} mask {mask}: record {p:?} axis {a}");
            }
            assert_eq!(got[i].velocity[3], 7.0, "the owner code stays");
            assert_eq!(got[i].weight, moving[i].weight, "the friction stays");
        }
        println!("GPU FLIP pockets seed {seed:#x} mask {mask}: {} isolated cells, {} pockets, {rounds} rounds", isolated.iter().filter(|&&s| s).count(), sums.len());
    }
    assert!(zeroed > 0, "some pocket was sealed");
}

/// Pocket sweeps' original triples and appended replay range agree through
/// changed, quiet and inactive rounds, even with a stale active range.
#[test]
fn gpu_flip_pocket_round_initializes_replay_gate() {
    fn write(pass: &Pass, binding: u32, values: &[u32]) {
        let buffer = &pass.bound.iter().find(|(b, _)| *b == binding).expect("bound").1;
        assert_eq!(buffer.size, (values.len() * 4) as u64);
        // SAFETY: each Pass::run retired its command buffer before this write.
        unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
    }
    let params = lattice();
    let words = POCKET_GATE_WORDS as usize;
    assert_eq!(words, 17, "eleven original words, pad, two replay range words and setup triple");
    let mut clock = [0u32; 12];
    clock[0] = (1.0f32 / 60.0).to_bits();
    clock[11] = 1;
    let mut pass = Pass::new();
    pass.bind(24, &vec![73u32; words]).bind(46, &clock);
    let mut expected = vec![73u32; words];
    expected[9] = 1;
    expected[10] = 0;
    expected[12..14].copy_from_slice(&[0, 0]);
    expected[14..17].copy_from_slice(&[0, 1, 1]);
    assert_eq!(pass.run::<u32>("pocket_start", &params, 24, words, 1), expected, "active start clears the range and preserves original triple words");
    let lines = [N[1] * N[2], N[2] * N[0], N[0] * N[1]];
    for (axis, count) in lines.into_iter().enumerate() {
        expected[3 * axis..3 * axis + 3].copy_from_slice(&[count.div_ceil(256) as u32, 1, 1]);
    }
    expected[9] = 0;
    expected[12..14].copy_from_slice(&[0, 4]);
    expected[14] = 1;
    assert_eq!(pass.run::<u32>("pocket_round", &params, 24, words, 1), expected, "changed round enables three sweeps and next setup");
    for axis in 0..3 { expected[3 * axis] = 0; }
    expected[12..14].copy_from_slice(&[0, 0]);
    expected[14] = 0;
    assert_eq!(pass.run::<u32>("pocket_round", &params, 24, words, 1), expected, "quiet round enables no sweep");

    // An inactive slot always starts before its rounds. Start clears the
    // preceding active slot's range; each inactive round keeps it cleared.
    let mut stale = vec![73u32; words];
    stale[9] = 1;
    stale[12..14].copy_from_slice(&[0, 4]);
    stale[14..17].copy_from_slice(&[1, 1, 1]);
    write(&pass, 24, &stale);
    clock[0] = 0;
    write(&pass, 46, &clock);
    for axis in 0..3 { stale[3 * axis] = 0; }
    stale[9] = 0;
    stale[10] = 0;
    stale[12..14].copy_from_slice(&[0, 0]);
    stale[14] = 0;
    assert_eq!(pass.run::<u32>("pocket_start", &params, 24, words, 1), stale, "inactive start clears stale execution and retains original word semantics");
    assert_eq!(pass.run::<u32>("pocket_round", &params, 24, words, 1), stale, "inactive round preserves the cleared execution range");
}

#[test]
fn gpu_flip_pocket_spread_reports_an_unfinished_cap() {
    // Every cell water and every link open, air only past the open -X face:
    // with no round run, sealed cells still link to air.
    let params = StepParams { closed_faces: 63 & !1, ..lattice() };
    let water = vec![1.0f32; cell_len()];
    let open = solid_faces(0x5e9, false);
    let mut pass = Pass::new();
    pass.bind(6, &water).bind(10, &open).bind(23, &vec![0u32; cell_len()]).bind(24, &[0u32; POCKET_GATE_WORDS as usize]).bind(25, &vec![0u32; cell_len()]);
    pass.run::<u32>("pocket_seed", &params, 23, cell_len(), cell_len());
    pass.run::<u32>("pocket_start", &params, 24, POCKET_GATE_WORDS as usize, 1);
    pass.run::<u32>("pocket_check", &params, 24, POCKET_GATE_WORDS as usize, cell_len());
    assert_eq!(pass.bound::<u32>(24, POCKET_GATE_WORDS as usize)[10], 1, "an unfinished spread is flagged");
    // The step's dry, sealed and air counts follow in words 7-9.
    let pocket = pass.bound::<u32>(23, cell_len());
    let counts: Vec<u32> = (0..3).map(|k| pocket.iter().filter(|&&s| s == k).count() as u32).collect();
    assert_eq!(counts, [0, (cell_len() - N[1] * N[2]) as u32, (N[1] * N[2]) as u32], "every cell water, the -X layer touching air");
    pass.bind(1, &vec![0u32; 2 * cell_len()]).bind(7, &vec![0.0f32; cell_len()]);
    pass.bind(22, &[5u32, 6, 7, 9, 0, 0, 0, 77, 77, 77, 77, 77, 77, 77, 77, 77, 77]);
    let first: Vec<u32> = pass.run("pocket_tally", &StepParams { step_in_tick: 0, ..params }, 22, 17, 1);
    assert_eq!(first[..4], [5, 6, 7, 1], "the tick's first step sets the word");
    assert_eq!(first[7..10], counts[..], "the step's dry, sealed and air cells");
    assert_eq!(first[10..14], [0, u32::MAX, 0, 0], "the lowest seed is cell 0, through the open -X box face");
    assert_eq!(first[16], 0, "every cell water: no dry floor hole");
    let second: Vec<u32> = pass.run("pocket_tally", &StepParams { step_in_tick: 1, ..params }, 22, 17, 1);
    assert_eq!(second[..4], [5, 6, 7, 2], "later steps add to it");
    assert_eq!(super::super::gpu_flip_step::pocket_rounds([6, 5, 4]), 6, "the cap is the longest side");
}

/// The fine pockets' states and labels after a settled spread over `water`
/// and `open`, every tank face closed.
fn fine_pockets(water: &[f32], open: &[FaceSample]) -> (Vec<u32>, Vec<u32>) {
    let params = lattice();
    let lines = (N[1] * N[2]).max(N[0] * N[2]).max(N[0] * N[1]);
    let mut pass = Pass::new();
    pass.bind(6, water).bind(10, open).bind(23, &vec![0u32; cell_len()]).bind(24, &[0u32; POCKET_GATE_WORDS as usize]).bind(25, &vec![0u32; cell_len()]);
    pass.run::<u32>("pocket_seed", &params, 23, cell_len(), cell_len());
    pass.run::<u32>("pocket_start", &params, 24, POCKET_GATE_WORDS as usize, 1);
    for _ in 0..=cell_len() {
        pass.run::<u32>("pocket_round", &params, 24, POCKET_GATE_WORDS as usize, 1);
        for sweep in ["pocket_sweep_x", "pocket_sweep_y", "pocket_sweep_z"] {
            pass.run::<u32>(sweep, &params, 24, POCKET_GATE_WORDS as usize, lines);
        }
        if pass.bound::<u32>(24, POCKET_GATE_WORDS as usize)[9] == 0 {
            break;
        }
    }
    pass.run::<u32>("pocket_check", &params, 24, POCKET_GATE_WORDS as usize, cell_len());
    assert_eq!(pass.bound::<u32>(24, POCKET_GATE_WORDS as usize)[10], 0, "a settled spread leaves no sealed cell linked to air");
    (pass.bound(23, cell_len()), pass.bound(25, cell_len()))
}

/// The pockets at Solve Level 1 as the CPU coarsens them: a coarse cell is
/// sealed when every in-lattice child that is not solid is sealed under one
/// fine label (a solid child is skipped, as the solver coarsens it), and
/// takes the lowest coarse cell of that fine label as its own; (state,
/// label) per coarse cell, the label of a dry cell unspecified (None).
fn cpu_coarse_pockets(labels: &[Option<usize>], solid: &[bool], c: [usize; 3]) -> Vec<Option<usize>> {
    let coarse_len = c.iter().product::<usize>();
    let fine_label: Vec<Option<usize>> = (0..coarse_len)
        .map(|i| {
            let at = [i % c[0], (i / c[0]) % c[1], i / (c[0] * c[1])];
            let mut shared = None;
            for child in 0..8 {
                let q = [2 * at[0] + (child & 1), 2 * at[1] + (child >> 1 & 1), 2 * at[2] + (child >> 2)];
                if (0..3).any(|a| q[a] >= N[a]) || solid[cell_index(q)] {
                    continue;
                }
                match (labels[cell_index(q)], shared) {
                    (None, _) => return None,
                    (Some(l), None) => shared = Some(l),
                    (Some(l), Some(s)) if l != s => return None,
                    _ => {}
                }
            }
            shared
        })
        .collect();
    (0..coarse_len)
        .map(|i| fine_label[i].map(|l| (0..coarse_len).find(|&j| fine_label[j] == Some(l)).expect("itself at the latest")))
        .collect()
}

/// At Solve Level 1 the pockets are coarsened with the water
/// (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11 (Solve Level)): a
/// level cell is sealed when all its fine children are sealed under one
/// label, its label the lowest level cell of that pocket, and the mean
/// comes off the coarse right-hand side pocket by pocket so each sums to
/// zero again; what it removes is h³ at the coarse cell size. A tank of
/// water, whole; split by a closed plane on a coarse boundary (two coarse
/// pockets); split off it (the straddling coarse cells stay dry); with one
/// solid cell (not water, every face closed) inside the first coarse cell,
/// which stays sealed with its seven water children.
#[test]
fn gpu_flip_pocket_mean_at_solve_level_one_sums_to_zero() {
    let coarse = N.map(|n| n.div_ceil(2));
    assert_eq!(coarse, [3, 3, 2]);
    let coarse_len = coarse.iter().product::<usize>();
    let mut sealed_coarse_total = 0;
    for (case, split_x, solid_cell) in [
        ("whole", None, None),
        ("split on the coarse boundary", Some(4), None),
        ("split through a coarse cell", Some(3), None),
        ("a solid cell inside a coarse cell", None, Some([1, 1, 1])),
    ] {
        let mut water = vec![1.0f32; cell_len()];
        let mut open = solid_faces(0x5ea, false);
        if let Some(x) = split_x {
            for (i, face) in open.iter_mut().enumerate() {
                if pad_coords(i)[0] == x {
                    face.weight[0] = 0.0;
                }
            }
        }
        if let Some(c) = solid_cell {
            water[cell_index(c)] = 0.0;
            for (i, face) in open.iter_mut().enumerate() {
                let p = pad_coords(i);
                for a in 0..3 {
                    if (0..3).all(|b| b == a || p[b] == c[b]) && (p[a] == c[a] || p[a] == c[a] + 1) {
                        face.weight[a] = 0.0;
                    }
                }
            }
        }
        let solid: Vec<bool> = (0..cell_len()).map(|i| solid_cell == Some(cell_coords(i))).collect();
        let (state, label) = fine_pockets(&water, &open);
        let (_, sealed) = cpu_isolated(&water, &open, 63);
        assert!((0..cell_len()).all(|i| sealed[i] || solid[i]), "{case}: every water cell is sealed");
        let fine_labels = cpu_pocket_labels(&sealed, &open);
        for i in 0..cell_len() {
            if solid[i] {
                assert_ne!(state[i], 1, "{case}: the solid cell {i} is no pocket");
                continue;
            }
            assert_eq!(state[i], 1, "{case}: fine cell {i} sealed");
            assert_eq!(Some(label[i] as usize), fine_labels[i], "{case}: fine cell {i} label");
        }
        let params = StepParams { solve_level: 1, ..lattice() };
        let mut pass = Pass::new();
        pass.bind(6, &water).bind(10, &open).bind(23, &state).bind(25, &label).bind(39, &vec![7u32; coarse_len]).bind(40, &vec![7u32; coarse_len]).bind(41, &vec![0u32; cell_len()]);
        pass.run::<u32>("pocket_leader_clear", &params, 41, cell_len(), cell_len());
        pass.run::<u32>("pocket_coarsen", &params, 39, coarse_len, coarse_len);
        let got_label = pass.run::<u32>("pocket_relabel", &params, 40, coarse_len, coarse_len);
        let got_state = pass.bound::<u32>(39, coarse_len);
        let want = cpu_coarse_pockets(&fine_labels, &solid, coarse);
        if solid_cell.is_some() {
            assert!(want[0].is_some(), "{case}: the coarse cell over the solid cell is sealed");
        }
        for i in 0..coarse_len {
            assert_eq!(got_state[i] == 1, want[i].is_some(), "{case}: coarse cell {i} sealed");
            if let Some(l) = want[i] {
                assert_eq!(got_label[i] as usize, l, "{case}: coarse cell {i} label");
            }
        }
        let sealed_count = want.iter().flatten().count();
        sealed_coarse_total += sealed_count;
        let pockets: std::collections::BTreeSet<usize> = want.iter().flatten().copied().collect();
        println!("coarse pockets {case}: {sealed_count} of {coarse_len} coarse cells sealed in {} pockets {pockets:?}", pockets.len());
        // The mean off the coarse right-hand side, on the coarse lattice at
        // its cell size, the flux into the pressure word of a later substep.
        let mut rng = Stream::new(0x5eb);
        let rhs: Vec<f32> = (0..coarse_len).map(|_| 4.0 * rng.unit() - 1.0).collect();
        let coarse_params = StepParams { n: coarse.map(|n| n as u32), cell_size: 2.0 * H, step_in_tick: 1, ..params };
        let mut mean = Pass::new();
        mean.bind(23, &got_state).bind(25, &got_label).bind(5, &rhs).bind(26, &vec![7u32; 3 * coarse_len + 2]).bind(22, &[9u32; 6]);
        mean.run::<u32>("pocket_clear", &coarse_params, 26, 3 * coarse_len + 2, 3 * coarse_len + 2);
        mean.run::<u32>("pocket_accumulate", &coarse_params, 26, 3 * coarse_len + 2, coarse_len);
        let removed = mean.run::<f32>("pocket_remove", &coarse_params, 5, coarse_len, coarse_len);
        let words = mean.run::<u32>("pocket_flux_pressure", &coarse_params, 22, 6, 1);
        let mut total = 0.0;
        for &l in &pockets {
            let members: Vec<usize> = (0..coarse_len).filter(|&i| want[i] == Some(l)).collect();
            let sum: f64 = members.iter().map(|&i| f64::from(rhs[i])).sum();
            total += sum.abs();
            for &i in &members {
                let want_value = f64::from(rhs[i]) - sum / members.len() as f64;
                assert!((f64::from(removed[i]) - want_value).abs() <= 1e-4, "{case}: coarse cell {i} rhs {} want {want_value}", removed[i]);
            }
            let left: f64 = members.iter().map(|&i| f64::from(removed[i])).sum();
            assert!(left.abs() < 1e-3, "{case}: coarse pocket {l} sums to {left} after its mean is removed");
        }
        for i in (0..coarse_len).filter(|&i| want[i].is_none()) {
            assert_eq!(removed[i], rhs[i], "{case}: dry coarse cell {i} keeps its value");
        }
        let h = 2.0 * f64::from(H);
        let flux = f64::from(f32::from_bits(words[4])) - f64::from(f32::from_bits(9));
        let want_flux = total * h * h * h;
        assert!((flux - want_flux).abs() <= 1e-4 * want_flux.max(1e-9), "{case}: removed flux {flux}, want {want_flux} at the coarse cell size");
    }
    assert!(sealed_coarse_total > 0);
}

