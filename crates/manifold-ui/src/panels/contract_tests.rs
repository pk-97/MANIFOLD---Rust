//! Interaction contracts every panel of a kind must honour, checked once across
//! all hosts. Each panel's own `tests` module supplies a fixture that builds
//! the panel and performs the gesture; the contract is asserted here.

use super::{PanelAction, Panel, ScrubPhase, ScrubValue, ValueRef};
use super::footer::FooterPanel;
use super::header::HeaderPanel;
use super::transport::TransportPanel;
use crate::intent::{Gesture, IntentRegistry};
use crate::layout::ScreenLayout;
use crate::RootAction;
use crate::tree::UITree;

/// Header, transport, and footer clicks resolve through the intent registry
/// to the right transport action, or to none.
struct ChromeCase {
    panel: &'static str,
    make: fn() -> Box<dyn Panel>,
    /// (visible label, expected action as `Debug`, or `None` for no action).
    clicks: &'static [(&'static str, Option<&'static str>)],
}

#[test]
fn chrome_clicks_resolve_through_the_intent_registry() {
    let cases = [
        ChromeCase {
            panel: "footer",
            make: || Box::new(FooterPanel::new()),
            clicks: &[("60", Some("Transport(FpsFieldClicked)"))],
        },
        ChromeCase {
            panel: "header",
            make: || Box::new(HeaderPanel::new()),
            clicks: &[("+", Some("Transport(ZoomIn)"))],
        },
        ChromeCase {
            panel: "transport",
            make: || Box::new(TransportPanel::new()),
            clicks: &[
                ("PLAY", Some("Transport(PlayPause)")),
                ("SYNC", Some("Transport(ToggleSyncOutput)")),
                ("120.0", Some("Transport(BpmFieldClicked)")),
                // Clock authority is display-only: interactive, no action.
                ("SRC:INT", None),
                ("ARM", Some("Transport(ToggleAutomationArm)")),
                ("RESTORE ALL", Some("Transport(AutomationBackToArrangement)")),
                ("AUTOMATION", Some("Transport(ToggleAutomationMode)")),
            ],
        },
    ];
    for case in &cases {
        let mut tree = UITree::new();
        let mut panel = (case.make)();
        panel.build(&mut tree, &ScreenLayout::new(1920.0, 1080.0));
        let mut intents = IntentRegistry::new();
        panel.register_intents(&mut intents);

        assert!(intents.resolve(&tree, None, Gesture::Click).is_none(), "{}: no node, no action", case.panel);
        for &(label, want) in case.clicks {
            let id = (0..tree.count())
                .filter_map(|i| tree.get_node(tree.id_at(i)))
                .find(|n| n.text.as_deref() == Some(label))
                .map(|n| n.id)
                .unwrap_or_else(|| panic!("{}: no {label:?} node", case.panel));
            let got = intents.resolve(&tree, Some(id), Gesture::Click).map(|a| format!("{a:?}"));
            assert_eq!(got.as_deref(), want, "{}: click {label:?}", case.panel);
        }
    }
}

/// One slider's right-click, as a panel fixture performed it.
pub(super) struct ResetCase {
    pub label: String,
    /// What the right-click resolved to.
    pub got: Option<PanelAction>,
    /// The value the reset must scrub.
    pub target: fn(&ValueRef) -> bool,
    /// The slider's declared default, which the reset must land on.
    pub default: f32,
}

/// Right-click on any slider track resets it: a `SliderReset` wrapping a
/// scrub move of that slider's own value to its declared default, never the
/// surrounding row's context menu.
#[test]
fn every_slider_host_right_click_resets_to_declared_default() {
    type Host = (&'static str, fn() -> Vec<ResetCase>);
    let hosts: [Host; 6] = [
        ("param card", super::param_card::tests::right_click_resets),
        ("layer chrome", super::layer_chrome::tests::right_click_resets),
        ("master chrome", super::master_chrome::tests::right_click_resets),
        ("macros", super::macros_panel::tests::right_click_resets),
        ("layer header", super::layer_header::tests::right_click_resets),
        ("audio setup", super::audio_setup_panel::tests::right_click_resets),
    ];
    for (host, fixture) in hosts {
        let cases = fixture();
        assert!(!cases.is_empty(), "{host}: no sliders");
        for case in cases {
            let what = format!("{host} / {}", case.label);
            let Some(PanelAction::Root(RootAction::SliderReset { changed, .. })) = case.got else {
                panic!("{what}: expected SliderReset, got {:?}", case.got);
            };
            let PanelAction::Scrub(ref value, ScrubPhase::Move(ScrubValue::Scalar(v))) = *changed else {
                panic!("{what}: expected a scalar scrub move, got {changed:?}");
            };
            assert!((case.target)(value), "{what}: reset scrubs the wrong value {value:?}");
            assert!((v - case.default).abs() < 1e-6, "{what}: reset lands on {v}, declared default {}", case.default);
        }
    }
}

/// Clicking a card's chevron emits exactly that card's collapse toggle.
#[test]
fn every_chevron_click_emits_its_collapse_toggle() {
    type Chevron = (&'static str, fn() -> Vec<PanelAction>, fn(&PanelAction) -> bool);
    let cases: [Chevron; 5] = [
        ("param card", super::param_card::tests::chevron_click, |a| {
            matches!(a, PanelAction::Params(crate::ParamsAction::EffectCollapseToggle(0)))
        }),
        ("layer chrome", super::layer_chrome::tests::chevron_click, |a| {
            matches!(a, PanelAction::Params(crate::ParamsAction::LayerChromeCollapseToggle))
        }),
        ("master chrome", super::master_chrome::tests::chevron_click, |a| {
            matches!(a, PanelAction::Params(crate::ParamsAction::MasterCollapseToggle))
        }),
        ("macros", super::macros_panel::tests::chevron_click, |a| {
            matches!(a, PanelAction::Params(crate::ParamsAction::MacrosCollapseToggle))
        }),
        ("layer header", super::layer_header::tests::chevron_click, |a| {
            matches!(a, PanelAction::Layer(crate::LayerAction::ChevronClicked(id)) if id.as_str() == "L1")
        }),
    ];
    for (host, click, is_toggle) in cases {
        let actions = click();
        assert!(
            matches!(actions.as_slice(), [a] if is_toggle(a)),
            "{host}: chevron click emitted {actions:?}"
        );
    }
}
