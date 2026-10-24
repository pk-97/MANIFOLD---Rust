//! The one copier for a host param's modulation routes (drivers, envelopes,
//! audio mods, automation lanes and Ableton mappings) when params are copied
//! or moved to new ids: duplicates, clipboard pastes and owner replacement.

use std::borrow::Cow;

use manifold_core::effects::PresetInstance;

/// What a route transfer does with Ableton mappings. A copy leaves them on
/// the source, so one hardware control keeps driving one target, as
/// Ableton's own duplicate leaves MIDI and key mappings behind. A move
/// (the same params under a replaced owner) takes them along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteTransfer {
    Copy,
    Move,
}

/// Copy `source`'s routes for each `(source id, destination id)` remap into
/// `destination`, replacing any route the destination ids already had.
pub fn copy_param_routes(
    source: &PresetInstance,
    destination: &mut PresetInstance,
    remaps: &[(String, String)],
    transfer: RouteTransfer,
) {
    macro_rules! copy_routes {
        ($field:ident) => {
            if let Some(entries) = &source.$field {
                let copies: Vec<_> = entries
                    .iter()
                    .flat_map(|entry| {
                        remaps
                            .iter()
                            .filter(move |(source_id, _)| entry.param_id.as_ref() == source_id)
                            .map(move |(_, destination_id)| {
                                let mut copy = entry.clone();
                                copy.param_id = Cow::Owned(destination_id.clone());
                                copy
                            })
                    })
                    .collect();
                if !copies.is_empty() {
                    let destination_entries = destination.$field.get_or_insert_with(Vec::new);
                    destination_entries.retain(|entry| {
                        !remaps
                            .iter()
                            .any(|(_, destination_id)| entry.param_id.as_ref() == destination_id)
                    });
                    destination_entries.extend(copies);
                }
            }
        };
    }
    copy_routes!(drivers);
    copy_routes!(envelopes);
    copy_routes!(audio_mods);
    copy_routes!(automation_lanes);
    if transfer == RouteTransfer::Move {
        copy_routes!(ableton_mappings);
    }
}
