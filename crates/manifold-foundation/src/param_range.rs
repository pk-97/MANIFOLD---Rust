//! What a control's value may never pass, kept apart from what its slider
//! spans (SCENE_SURFACES_DESIGN.md D15 (slider range and hard limit)).

use serde::{Deserialize, Serialize};

/// The hard limit on a control's value. `Slider` means the limit is the
/// declared slider range, which is how every control behaved before D15.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "value")]
pub enum HardLimit {
    #[default]
    Slider,
    Unlimited,
    AtLeast(f32),
    AtMost(f32),
    Between(f32, f32),
}

impl HardLimit {
    pub fn is_slider(&self) -> bool {
        matches!(self, Self::Slider)
    }

    /// Clamp `v` to this limit; `declared` is the control's declared range,
    /// never the user's slider choice.
    pub fn clamp(self, v: f32, declared: (f32, f32)) -> f32 {
        match self {
            Self::Slider => v.clamp(declared.0, declared.1),
            Self::Unlimited => v,
            Self::AtLeast(lo) => v.max(lo),
            Self::AtMost(hi) => v.min(hi),
            Self::Between(lo, hi) => v.clamp(lo, hi),
        }
    }

    /// Clamp a typed or OSC value: only a declared limit restricts it. Under
    /// `Slider` it passes through, since a range hint never restricts what a
    /// user enters (PARAM_RANGE_CONTRACT_DESIGN.md D3 (hints never restrict)).
    pub fn clamp_input(self, v: f32, declared: (f32, f32)) -> f32 {
        if self.is_slider() {
            v
        } else {
            self.clamp(v, declared)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HardLimit;

    #[test]
    fn clamp_follows_each_limit() {
        let slider = (0.0, 1.0);
        assert_eq!(HardLimit::Slider.clamp(2.0, slider), 1.0);
        assert_eq!(HardLimit::Unlimited.clamp(-5.0, slider), -5.0);
        assert_eq!(HardLimit::AtLeast(0.0).clamp(-1.0, slider), 0.0);
        assert_eq!(HardLimit::AtLeast(0.0).clamp(9.0, slider), 9.0);
        assert_eq!(HardLimit::AtMost(3.0).clamp(9.0, slider), 3.0);
        assert_eq!(HardLimit::Between(1.0, 179.0).clamp(200.0, slider), 179.0);
    }

    #[test]
    fn slider_is_the_default_and_round_trips() {
        assert_eq!(HardLimit::default(), HardLimit::Slider);
        for limit in [
            HardLimit::Slider,
            HardLimit::AtLeast(0.0),
            HardLimit::Between(1.0, 2.0),
        ] {
            let json = serde_json::to_string(&limit).unwrap();
            assert_eq!(serde_json::from_str::<HardLimit>(&json).unwrap(), limit);
        }
    }
}
