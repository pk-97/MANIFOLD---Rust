use super::*;

fn flow(name: &str) -> Vec<AutomationAction> {
    let path = format!("{}/../../scripts/ui-flows/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).expect("read flow")).expect("parse flow")
}

/// Mirrors `run_flow`: flush before a reader, a `Step` composites and
/// tiles every frame, a `Snapshot` advances nothing, every other step
/// advances one frame. Returns (composites, filmstrip tiles).
fn composites_for(actions: &[AutomationAction]) -> (usize, usize) {
    let mut gate = CompositeGate::default();
    gate.mark();
    let mut tiles = 0;
    for action in actions {
        if reads_composite(action) {
            gate.take();
        }
        match action {
            AutomationAction::Step { frames } => {
                for _ in 0..*frames {
                    gate.step_frame();
                    tiles += 1;
                }
            }
            AutomationAction::Snapshot => {}
            _ => gate.mark(),
        }
    }
    (gate.composites, tiles)
}

#[test]
fn step_composites_and_tiles_every_frame() {
    let actions = [AutomationAction::Dump, AutomationAction::Step { frames: 5 }];
    // Seed + the Dump frame are owed but folded into the first Step frame.
    assert_eq!(composites_for(&actions), (5, 5));
    let actions = [AutomationAction::Step { frames: 3 }, AutomationAction::Step { frames: 2 }];
    assert_eq!(composites_for(&actions), (5, 5));
}

#[test]
fn run_without_snapshot_steps_never_composites() {
    let mut actions = flow("water-family-preset");
    actions.retain(|a| !matches!(a, AutomationAction::Step { .. }));
    let snapshots = actions.iter().filter(|a| reads_composite(a)).count();
    assert!(snapshots > 0, "fixture flow should carry Snapshot steps");
    assert_eq!(composites_for(&actions).0, snapshots);
    actions.retain(|a| !reads_composite(a));
    assert!(actions.len() > 20);
    assert_eq!(composites_for(&actions).0, 0);
}

#[test]
fn only_snapshot_reads_the_composite() {
    let non_readers = [
        AutomationAction::Dump,
        AutomationAction::Step { frames: 3 },
        AutomationAction::SaveReloadProject,
    ];
    assert!(non_readers.iter().all(|a| !reads_composite(a)));
    assert!(reads_composite(&AutomationAction::Snapshot));
}

#[test]
fn back_to_back_snapshots_composite_once() {
    let mut gate = CompositeGate::default();
    gate.mark();
    assert!(gate.take());
    assert!(!gate.take());
    assert_eq!(gate.composites, 1);
}
