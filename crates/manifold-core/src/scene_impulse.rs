//! Scene-space recipient selections for authored physics impulses.

/// Live tick-start diagnostics. These counters are not a recorded physics take.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SceneImpulseDiagnostics {
    pub started: u64,
    pub late: u64,
    /// Hits fired while the simulation was held (pause, Speed 0).
    pub discarded: u64,
}

/// A fixed set of ordinary body slots and the reset-latched copy group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RigidImpulseTargets {
    pub bodies: u64,
    pub copies: bool,
}

impl RigidImpulseTargets {
    /// Number of ordinary body slots represented by the selection mask.
    pub const BODY_CAPACITY: usize = u64::BITS as usize;

    pub const fn is_empty(self) -> bool {
        self.bodies == 0 && !self.copies
    }

    pub const fn contains_body(self, index: usize) -> bool {
        index < Self::BODY_CAPACITY && (self.bodies & (1u64 << index)) != 0
    }
}

/// Which native simulation should receive a resolved graph impulse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImpulseTarget {
    Rigid(RigidImpulseTargets),
    Fluid,
    /// One captured field delivered to both participants of a shared worker.
    /// The source sequence is admitted once, even when both materials respond.
    FluidAndRigid(RigidImpulseTargets),
}

impl ImpulseTarget {
    pub fn affects_fluid(self) -> bool {
        matches!(self, Self::Fluid | Self::FluidAndRigid(_))
    }

    pub fn rigid_targets(self) -> Option<RigidImpulseTargets> {
        match self {
            Self::Rigid(targets) | Self::FluidAndRigid(targets) => Some(targets),
            Self::Fluid => None,
        }
    }

    /// Merge authored selections which resolve to one shared native owner.
    pub fn union(self, other: Self) -> Self {
        let fluid = self.affects_fluid() || other.affects_fluid();
        let rigid = match (self.rigid_targets(), other.rigid_targets()) {
            (Some(left), Some(right)) => Some(RigidImpulseTargets {
                bodies: left.bodies | right.bodies,
                copies: left.copies || right.copies,
            }),
            (left, right) => left.or(right),
        };
        match (fluid, rigid) {
            (true, Some(targets)) => Self::FluidAndRigid(targets),
            (true, None) => Self::Fluid,
            (false, Some(targets)) => Self::Rigid(targets),
            (false, None) => unreachable!("each input has at least one recipient"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ImpulseTarget, RigidImpulseTargets};

    #[test]
    fn body_mask_uses_the_full_u64_capacity() {
        let targets = RigidImpulseTargets {
            bodies: 1u64 << 63,
            copies: false,
        };
        assert_eq!(RigidImpulseTargets::BODY_CAPACITY, 64);
        assert!(targets.contains_body(63));
        assert!(!targets.contains_body(64));
        assert!(!targets.contains_body(usize::MAX));
    }

    #[test]
    fn recipient_serde_shapes_round_trip_exactly() {
        let rigid = RigidImpulseTargets {
            bodies: 1u64 << 63,
            copies: true,
        };
        let rigid_json = serde_json::to_string(&rigid).expect("rigid targets serialize");
        assert_eq!(
            rigid_json,
            r#"{"bodies":9223372036854775808,"copies":true}"#
        );
        assert_eq!(
            serde_json::from_str::<RigidImpulseTargets>(&rigid_json).unwrap(),
            rigid
        );

        let cases = [
            (ImpulseTarget::Fluid, r#""fluid""#),
            (
                ImpulseTarget::Rigid(RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                }),
                r#"{"rigid":{"bodies":1,"copies":false}}"#,
            ),
            (
                ImpulseTarget::FluidAndRigid(RigidImpulseTargets {
                    bodies: 2,
                    copies: true,
                }),
                r#"{"fluidAndRigid":{"bodies":2,"copies":true}}"#,
            ),
        ];

        for (value, expected) in cases {
            let json = serde_json::to_string(&value).expect("recipient serializes");
            assert_eq!(json, expected);
            assert_eq!(serde_json::from_str::<ImpulseTarget>(&json).unwrap(), value);
        }
    }

    #[test]
    fn union_preserves_each_recipient_class() {
        let rigid = RigidImpulseTargets {
            bodies: 1,
            copies: false,
        };
        let combined =
            ImpulseTarget::Rigid(rigid).union(ImpulseTarget::FluidAndRigid(RigidImpulseTargets {
                bodies: 1u64 << 4,
                copies: true,
            }));
        assert_eq!(
            combined,
            ImpulseTarget::FluidAndRigid(RigidImpulseTargets {
                bodies: 0b1_0001,
                copies: true,
            })
        );
        assert_eq!(
            ImpulseTarget::Fluid.union(ImpulseTarget::Fluid),
            ImpulseTarget::Fluid
        );
        assert_eq!(
            ImpulseTarget::Rigid(rigid).union(ImpulseTarget::Rigid(rigid)),
            ImpulseTarget::Rigid(rigid)
        );
    }
}
