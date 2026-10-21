use super::*;

#[test]
fn bend_update_preserves_curved_range_bounds() {
    assert_eq!(
        shape_with_bend(UiSegmentShape::CurvedRange { bend: 0.4, start: 0.2, end: 0.8 }, -0.35),
        UiSegmentShape::CurvedRange { bend: -0.35, start: 0.2, end: 0.8 }
    );
}

#[test]
fn pencil_sweep_replaces_reverse_interval_and_keeps_tails() {
    let mut points = vec![
        (Beats(0.0), 0.1, UiSegmentShape::Linear),
        (Beats(2.0), 0.2, UiSegmentShape::Linear),
        (Beats(4.0), 0.3, UiSegmentShape::Linear),
        (Beats(8.0), 0.8, UiSegmentShape::Linear),
    ];
    apply_draw_sweep(
        &mut points,
        Some((Beats(6.0), 0.6)),
        (Beats(2.0), 0.2),
        UiSegmentShape::Linear,
        1.0,
        false,
    );
    assert_eq!(points.first().map(|point| point.0), Some(Beats(0.0)));
    assert_eq!(points.last().map(|point| point.0), Some(Beats(8.0)));
    assert!(points.iter().any(|point| point.0 == Beats(2.0)));
    assert!(points.iter().any(|point| point.0 == Beats(6.0)));
    assert!(points.iter().any(|point| point.0 == Beats(4.0) && (point.1 - 0.4).abs() < 1e-6));
}

#[test]
fn pencil_replay_keeps_a_bent_three_sample_contour() {
    let original = vec![
        (Beats(0.0), 0.0, UiSegmentShape::CurvedRange { bend: 0.6, start: 0.15, end: 0.9 }),
        (Beats(8.0), 1.0, UiSegmentShape::Linear),
    ];
    let mut recorded = Vec::new();
    let mut working = Vec::new();
    for pair in [
        ((Beats(2.0), 0.2), (Beats(4.0), 0.9)),
        ((Beats(4.0), 0.9), (Beats(6.0), 0.1)),
    ] {
        apply_draw_sweep(
            &mut recorded,
            Some(pair.0),
            pair.1,
            UiSegmentShape::Linear,
            1.0,
            false,
        );
    }
    rebuild_draw_interval(&original, &mut working, &recorded);
    assert!(working.iter().any(|point| point.0 == Beats(4.0) && (point.1 - 0.9).abs() < f32::EPSILON));
    assert!(working.iter().any(|point| point.0 == Beats(6.0) && (point.1 - 0.1).abs() < f32::EPSILON));
    assert!(working.iter().any(|point| point.0 == Beats(8.0)));
    for index in 0..=160 {
        let beat = Beats(index as f64 / 20.0);
        if beat < Beats(2.0) || beat > Beats(6.0) {
            assert!((draw_value_at(&working, beat).unwrap() - draw_value_at(&original, beat).unwrap()).abs() < 1e-5);
        }
    }
    // Reverse only the end of the stroke, retaining the earlier peak.
    apply_draw_sweep(&mut recorded, Some((Beats(6.0), 0.1)), (Beats(5.0), 0.4), UiSegmentShape::Linear, 1.0, true);
    rebuild_draw_interval(&original, &mut working, &recorded);
    assert_eq!(draw_value_at(&working, Beats(4.0)), Some(0.9));
    assert_eq!(draw_value_at(&working, Beats(5.0)), Some(0.4));
}
