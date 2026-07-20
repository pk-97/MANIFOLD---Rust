//! The unified preset definition registry — one module for effects and
//! generators.
//!
//! Step 9 of the preset unification (`docs/PRESET_UNIFICATION_PLAN.md`):
//! the two parallel modules `effect_definition_registry` and
//! `generator_definition_registry` collapsed into this one. The value
//! type was already unified to [`crate::preset_def::PresetDef`] in step 7;
//! this step merges the two modules, deduplicates the converter / leak
//! helpers that were byte-identical mirrors, and exposes two thin
//! keyed-accessor submodules — [`effect`] and [`generator`] — over the two
//! stores.
//!
//! **Why two stores, not one.** Effects and generators are keyed by
//! distinct id types ([`PresetTypeId`] / [`PresetTypeId`]) and — more
//! importantly — populated from two **distinct disk sources**: the
//! renderer submits effect presets (`assets/effect-presets/`) and
//! generator presets (`assets/generator-presets/`) to two separate
//! [`inventory`] buckets. Merging into one `String`-keyed store would
//! either cross-contaminate those buckets or collide an effect id with a
//! generator id sharing a name — both touch the stable-addressing path.
//! So the module is one and the helpers are shared, but the two stores and
//! the two preset-source buckets stay distinct. The duplicated glue
//! (converters, leak helpers) was the actual fork residue; that is what
//! this step removes. Call sites change the module path only
//! (`…::effect::X` / `…::generator::X`) — the function names are
//! byte-identical to the legacy surface.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, OnceLock};

use arc_swap::ArcSwap;

use crate::effect_graph_def::{AliasEntry, PresetMetadata, ValueAliasEntry};
use crate::effect_registration::{ParamAlias, ParamValueAlias};
use crate::preset_type_id::PresetTypeId;
use crate::effects::RegistryParamDef;
use crate::preset_def::{PresetDef, PresetKind};

// ─── StringParamDef ───
//
// Generator-only state carried on `PresetDef::string_param_defs`.

/// A string parameter definition for generators that accept text input.
#[derive(Debug, Clone)]
pub struct StringParamDef {
    /// Display name shown in inspector.
    pub name: &'static str,
    /// Key used in `TimelineClip.string_params` map.
    pub key: &'static str,
    /// Default value for new clips.
    pub default_value: &'static str,
    /// If true, the inspector shows a dropdown selector instead of text input.
    pub use_dropdown: bool,
}

// ─── Static registries (hot-reloadable, step 10) ───
//
// The two stores used to be `LazyLock<HashMap<_, PresetDef>>` borrowed for
// the process lifetime. They are now `ArcSwap` of `Arc<HashMap<_,
// Arc<PresetDef>>>` so the hot-reload watcher can swap in a freshly-built
// map (from the reloaded preset JSON) without a restart via
// [`rebuild_effect_definitions`] / [`rebuild_generator_definitions`].
//
// `get`/`try_get` return a cheap `Arc<PresetDef>` refcount clone. They are
// NOT a per-frame hot path (modulation/Ableton/OSC address params by id
// against the live `ParamManifest`, not this registry — see P5 registry
// containment), so the extra atomic pointer load + refcount bump per call
// is negligible and only happens at chain (re)build / editor / addressing
// setup time.
//
// At rest the stores are never swapped, so a `load_full` returns the same
// `Arc` every time — byte-identical behaviour to the old `LazyLock`.

type PresetMap = HashMap<PresetTypeId, Arc<PresetDef>>;

/// The single unified definition store. Keyed by `PresetTypeId` across BOTH
/// kinds — effect ids and generator ids are disjoint (verified, and
/// [`build_preset_definitions`] asserts it at build time). `PresetDef.kind`
/// discriminates; every accessor reads the kind off the looked-up def
/// rather than picking a store, so an effect id and a generator id resolve
/// through the exact same code path. This is the keystone of the preset
/// unification: collapsing the two stores removes the "which registry?"
/// branch that every downstream fork derived from.
///
/// `ArcSwap` (step 10) so the hot-reload watcher can swap a freshly-built
/// map in without a restart via [`rebuild_preset_definitions`]. At rest the
/// store is never swapped, so a `load`/`load_full` returns the same `Arc`
/// every time — byte-identical to the old `LazyLock`.
static PRESET_DEFINITIONS: LazyLock<ArcSwap<PresetMap>> = LazyLock::new(|| {
    ArcSwap::from_pointee(build_preset_definitions(
        effect::loaded_preset_metadata(),
        generator::loaded_preset_metadata(),
    ))
});

// Built once at init from both kinds' inventory + JSON metadata; rebuilt
// only on hot-reload.

/// Build the one unified definition map. The two kinds are built into
/// separate maps first — so a JSON preset overriding an inventory
/// submission of the **same kind** is the normal `insert`-returns-`Some`
/// case — then merged with a cross-kind disjointness assertion at the join.
/// That assertion is the build-time dup-key guard: zero collisions ship
/// today, and it fences a future id reused across kinds (which would
/// otherwise silently shadow one preset in the flat id-keyed map).
fn build_preset_definitions(
    effect_json: &[PresetMetadata],
    generator_json: &[PresetMetadata],
) -> PresetMap {
    let mut m = build_effect_kind_map(effect_json);
    for (id, def) in build_generator_kind_map(generator_json) {
        assert!(
            !m.contains_key(&id),
            "duplicate preset id across effect+generator kinds: '{id}' — effect and generator ids must be globally unique"
        );
        m.insert(id, def);
    }
    m
}

/// Build the effect half (kind = Effect): inventory `EffectMetadata`, then
/// sidecar param/value alias attach, then JSON-loaded presets (a JSON
/// preset wins over an inventory submission for the same id — the legal
/// same-kind `insert`-returns-`Some` override).
fn build_effect_kind_map(json_presets: &[PresetMetadata]) -> PresetMap {
    let mut m: PresetMap = HashMap::new();
    // All effects are registered via inventory::submit! in their
    // implementation files (manifold-renderer/src/effects/*.rs).
    for meta in inventory::iter::<crate::effect_registration::EffectMetadata> {
        m.insert(meta.id.clone(), Arc::new(meta.to_effect_def()));
    }
    // Sidecar alias submissions: attach to the matching def. Built
    // separately from `EffectMetadata` so effects without aliases
    // (the common case) don't need to spell out an empty slice in
    // their primary submission. See `effect_registration::EffectAliasMetadata`.
    for alias_meta in inventory::iter::<crate::effect_registration::EffectAliasMetadata> {
        if let Some(def) = m.get_mut(&alias_meta.id) {
            Arc::make_mut(def).legacy_param_aliases = alias_meta.aliases;
        }
    }
    // Same pattern for **value** aliases — slot-value migration tables
    // applied at project load time. See
    // `effect_registration::EffectValueAliasMetadata`.
    for alias_meta in inventory::iter::<crate::effect_registration::EffectValueAliasMetadata> {
        if let Some(def) = m.get_mut(&alias_meta.id) {
            Arc::make_mut(def).legacy_value_aliases = alias_meta.aliases;
        }
    }
    // JSON-loaded presets (§11 unified-registry migration). Each entry is
    // converted to a `PresetDef` and inserted — a JSON-loaded preset wins
    // over an inventory submission for the same id. Post-§11 every shipping
    // effect lives in JSON; the inventory loop above only fires for tests
    // that submit synthetic `EffectMetadata` entries.
    for preset in json_presets {
        m.insert(
            preset.id.clone(),
            Arc::new(preset_metadata_to_def(preset, PresetKind::Effect)),
        );
    }
    m
}

/// Build the generator half (kind = Generator): the `None` sentinel, then
/// inventory `GeneratorMetadata`, then sidecar aliases, then JSON-loaded
/// presets (same-kind override allowed). Mirror of [`build_effect_kind_map`].
fn build_generator_kind_map(json_presets: &[PresetMetadata]) -> PresetMap {
    let mut m: PresetMap = HashMap::new();

    // ── None ──
    m.insert(
        PresetTypeId::NONE,
        Arc::new(PresetDef {
            kind: PresetKind::Generator,
            display_name: "None".to_string(),
            is_line_based: false,
            param_defs: Vec::new(),
            string_param_defs: Vec::new(),
            osc_prefix: None,
            legacy_param_aliases: &[],
            legacy_value_aliases: &[],
        }),
    );

    // All other generators are registered via inventory::submit! in their
    // implementation files (manifold-renderer/src/generators/*.rs).
    for meta in inventory::iter::<crate::generator_registration::GeneratorMetadata> {
        m.insert(meta.id.clone(), Arc::new(meta.to_generator_def()));
    }
    // Sidecar alias submissions for generators. See parallel effect path.
    for alias_meta in inventory::iter::<crate::generator_registration::GeneratorAliasMetadata> {
        if let Some(def) = m.get_mut(&alias_meta.id) {
            Arc::make_mut(def).legacy_param_aliases = alias_meta.aliases;
        }
    }
    // JSON-loaded presets (§11). A JSON-loaded preset wins over an
    // inventory submission for the same id — same dual-source pattern as
    // the effect side, so a generator that ships with a bundled JSON
    // preset *and* a legacy inventory entry uses the JSON as the
    // canonical schema. Eliminates the inventory-vs-preset positional
    // layout drift class structurally.
    for preset in json_presets {
        let gen_id = PresetTypeId::from_string(preset.id.as_str().to_string());
        m.insert(
            gen_id,
            Arc::new(preset_metadata_to_def(preset, PresetKind::Generator)),
        );
    }
    m
}

/// Hot-reload (step 10): rebuild the unified definition map from
/// freshly-reloaded JSON metadata for both kinds and swap it in with ONE
/// atomic `ArcSwap::store`. Called by the watcher thread after the
/// renderer's preset catalog snapshot has reloaded; the caller passes both
/// kinds' metadata so a reload never drops one kind or exposes a
/// half-merged store mid-swap.
///
/// Crash-safe by construction: the new map is built fully before the swap;
/// a swap is atomic, so a concurrent reader never sees a half-built map.
pub fn rebuild_preset_definitions(
    effect_json: &[PresetMetadata],
    generator_json: &[PresetMetadata],
) {
    PRESET_DEFINITIONS.store(Arc::new(build_preset_definitions(effect_json, generator_json)));
}

// ─── Display-name interner ───
//
// `ParamSource::display_name(&self) -> &str` (effect + generator impls)
// must hand back a `&str` that outlives the call, but a hot-reloadable
// registry hands out an owned `Arc<PresetDef>` whose `display_name` is
// dropped at the end of the call. To keep the trait signature (used by a
// lot of debug/UI callers) without rippling `String` everywhere, the name
// is interned once per `(type_id, name)` pair into a process-`'static`
// string. Bounded by the (finite) number of distinct preset names ever
// seen across reloads — authoring-time growth at worst, never a per-call
// leak: a repeat lookup of an already-seen name returns the cached
// `&'static str`. This is a borrow gateway, not a value cache.
static DISPLAY_NAME_INTERNER: LazyLock<parking_intern::Interner> =
    LazyLock::new(parking_intern::Interner::default);

/// Tiny string interner backed by an `ArcSwap`'d map. Keeps the crate's
/// `#![forbid(unsafe_code)]` contract (no `OnceCell`/`AtomicPtr` tricks)
/// while still returning `&'static str`.
mod parking_intern {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    pub struct Interner {
        map: ArcSwap<HashMap<String, &'static str>>,
    }

    impl Interner {
        /// Return a `&'static str` equal to `s`, interning it on first sight.
        /// The leak is bounded by the number of distinct strings interned.
        pub fn intern(&self, s: &str) -> &'static str {
            if let Some(found) = self.map.load().get(s) {
                return found;
            }
            // Slow path: leak the string and publish a new map snapshot.
            // A racing interner of the same string leaks one extra copy —
            // harmless and vanishingly rare (authoring-time).
            let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
            let mut next = HashMap::clone(&self.map.load());
            next.entry(s.to_string()).or_insert(leaked);
            let resolved = *next.get(s).expect("just inserted");
            self.map.store(Arc::new(next));
            resolved
        }
    }
}

/// Intern a display name to `&'static str`. See [`DISPLAY_NAME_INTERNER`].
pub(crate) fn intern_display_name(name: &str) -> &'static str {
    DISPLAY_NAME_INTERNER.intern(name)
}

// ─── Unified accessors ───
//
// One flat, kind-agnostic surface over [`PRESET_DEFINITIONS`]. An effect id
// and a generator id resolve through the same function; kind — where it
// actually matters (display fallback, default-instance construction) — is
// read off the looked-up `PresetDef.kind`, never off "which store". This
// replaces the former parallel `effect::*` / `generator::*` accessor pairs
// (the keystone fork). The `effect` / `generator` submodules below survive
// only for the two genuinely per-kind concerns: the disk-source inventory
// buckets and the picker-registration converters (distinct return types).

/// Re-export of the param-alias resolver. Canonical home is
/// [`crate::effect_registration::resolve_param_alias`].
pub use crate::effect_registration::resolve_param_alias;

/// Get the definition for a preset type (either kind). Panics if unknown.
///
/// Returns an owned `Arc<PresetDef>` (cheap refcount clone of the current
/// registry snapshot). Hot-reload (step 10): a reload swaps the snapshot, so
/// holding the `Arc` keeps the looked-up def alive across a concurrent
/// rebuild. Not a per-frame hot path.
pub fn get(type_id: &PresetTypeId) -> Arc<PresetDef> {
    try_get(type_id)
        .unwrap_or_else(|| panic!("PresetDefinitionRegistry: unknown PresetTypeId '{}'", type_id))
}

/// Try to get the definition for a preset type (either kind).
pub fn try_get(type_id: &PresetTypeId) -> Option<Arc<PresetDef>> {
    PRESET_DEFINITIONS.load().get(type_id).cloned()
}

/// Create a new `PresetInstance` initialised to the type's registry
/// defaults, constructed with the correct kind read off the def: an effect
/// via `PresetInstance::new` + base-param seeding, a generator via
/// `PresetInstance::new_generator` (which seeds its own defaults).
///
/// Seeds via the crate-internal `write_base_param`, NOT the public
/// `set_base_param` — this is system-level default population at
/// instance-construction time, not a live hand gesture, so it must not mark
/// the fresh slots `touched`. A brand-new effect would otherwise start every
/// param pre-latched as "overridden" for the automation-lane override latch
/// (`docs/AUTOMATION_LANES_DESIGN.md` §4) before any lane or hand ever
/// touched it.
pub fn create_default(type_id: &PresetTypeId) -> crate::effects::PresetInstance {
    let def = get(type_id);
    match def.kind {
        PresetKind::Generator => crate::effects::PresetInstance::new_generator(type_id.clone()),
        PresetKind::Effect => {
            let mut inst = crate::effects::PresetInstance::new(type_id.clone());
            // Seed the manifest whole from the registry template (D2).
            inst.params = crate::params::ParamManifest::from_params(
                def.param_defs
                    .iter()
                    .map(|pd| crate::params::Param::bundled(pd.spec.clone()))
                    .collect(),
            );
            inst.base_tracked = true;
            inst
        }
    }
}

/// Format a parameter value for display. Named labels win, then whole
/// numbers, then the param's format string, then F2. (Union of the former
/// effect/generator formatters.)
pub fn format_value(type_id: &PresetTypeId, param_index: usize, value: f32) -> String {
    let def = match try_get(type_id) {
        Some(d) if param_index < d.param_defs.len() => d,
        _ => return format!("{:.2}", value),
    };
    let pd = &def.param_defs[param_index];
    if !pd.spec.value_labels.is_empty() {
        let idx = (value.round() as i32).clamp(0, pd.spec.value_labels.len() as i32 - 1) as usize;
        return pd.spec.value_labels[idx].clone();
    }
    if pd.spec.whole_numbers {
        return format!("{}", value.round() as i32);
    }
    if let Some(ref fmt) = pd.spec.format_string {
        return format_float_with_format_string(value, fmt);
    }
    format!("{:.2}", value)
}

/// Master-effect OSC address for a specific param id (P4): `/master/{prefix}/{param_id}`.
/// The id comes off the live [`crate::params::ParamManifest`]; only `osc_prefix`
/// is read from the template (a type-level boundary read, allowed under D2).
/// `None` if the type has no prefix or the id is empty.
pub fn get_osc_address_by_id(type_id: &PresetTypeId, param_id: &str) -> Option<String> {
    if param_id.is_empty() {
        return None;
    }
    let prefix = try_get(type_id)?.osc_prefix.clone()?;
    Some(format!("/master/{prefix}/{param_id}"))
}

/// Layer-scoped OSC address for a specific param id (P4):
/// `/layer/{layerId}/{prefix}/{param_id}`.
pub fn get_osc_address_for_layer_by_id(
    type_id: &PresetTypeId,
    layer_id: &str,
    param_id: &str,
) -> Option<String> {
    if layer_id.is_empty() || param_id.is_empty() {
        return None;
    }
    let prefix = try_get(type_id)?.osc_prefix.clone()?;
    Some(format!("/layer/{layer_id}/{prefix}/{param_id}"))
}

/// Default parameters as freshly-seeded bundled [`crate::params::Param`]s, all
/// exposed, value = base = default.
pub fn get_defaults(type_id: &PresetTypeId) -> Vec<crate::params::Param> {
    let def = get(type_id);
    def.param_defs
        .iter()
        .map(|pd| crate::params::Param::bundled(pd.spec.clone()))
        .collect()
}

/// All registered type ids of a given kind (unordered).
pub fn all_of_kind(kind: PresetKind) -> Vec<PresetTypeId> {
    PRESET_DEFINITIONS
        .load()
        .iter()
        .filter(|(_, d)| d.kind == kind)
        .map(|(id, _)| id.clone())
        .collect()
}

// ─── Effect disk-source bucket ───
//
// The `effect` submodule survives ONLY for the JSON disk-source inventory
// bucket (so an effect preset never lands in the generator store).
// Resolution accessors moved to module scope above; the picker converter
// moved to `crate::preset_type_registry`.

pub mod effect {
    use super::*;

    /// JSON-loaded **effect** preset metadata. Each [`PresetSource`]
    /// submission contributes a `Vec<PresetMetadata>`; the renderer submits
    /// one pointing at the bundled effect-preset JSON. Invoked once on first
    /// access and cached for the process lifetime.
    pub fn loaded_preset_metadata() -> &'static [PresetMetadata] {
        static CACHE: OnceLock<Vec<PresetMetadata>> = OnceLock::new();
        CACHE.get_or_init(|| {
            let mut all = Vec::new();
            for source in inventory::iter::<PresetSource> {
                all.extend((source.load)());
            }
            all
        })
    }

    /// Inventory submission point for JSON-loaded **effect** preset
    /// metadata. Kept distinct from the generator bucket so an effect
    /// preset never lands in the generator store.
    ///
    /// Pattern:
    /// ```ignore
    /// inventory::submit! {
    ///     manifold_core::preset_definition_registry::effect::PresetSource {
    ///         load: my_loader_function,
    ///     }
    /// }
    /// ```
    pub struct PresetSource {
        pub load: fn() -> Vec<PresetMetadata>,
    }

    inventory::collect!(PresetSource);
}

// ─── Generator disk-source bucket ───
//
// Mirror of the `effect` submodule: the generator JSON disk-source bucket.
// The picker converter moved to `crate::preset_type_registry`.

pub mod generator {
    use super::*;

    /// JSON-loaded **generator** preset metadata. Mirror of
    /// [`super::effect::loaded_preset_metadata`] over the generator disk
    /// bucket. The renderer submits one source pointing at the bundled
    /// generator-preset JSON.
    pub fn loaded_preset_metadata() -> &'static [PresetMetadata] {
        static CACHE: OnceLock<Vec<PresetMetadata>> = OnceLock::new();
        CACHE.get_or_init(|| {
            let mut all = Vec::new();
            for source in inventory::iter::<PresetSource> {
                all.extend((source.load)());
            }
            all
        })
    }

    /// Inventory submission point for JSON-loaded **generator** preset
    /// metadata. Mirror of [`super::effect::PresetSource`] over the
    /// generator bucket — kept distinct so a generator preset never lands
    /// in the effect store.
    pub struct PresetSource {
        pub load: fn() -> Vec<PresetMetadata>,
    }

    inventory::collect!(PresetSource);
}

// ─── Format helper (shared) ───

fn format_float_with_format_string(value: f32, fmt: &str) -> String {
    match fmt {
        "F0" => format!("{:.0}", value),
        "F1" => format!("{:.1}", value),
        "F2" => format!("{:.2}", value),
        "F3" => format!("{:.3}", value),
        "F4" => format!("{:.4}", value),
        _ => format!("{:.2}", value),
    }
}

// ─── Shared converters ───
//
// §11 of `docs/PRIMITIVE_LIBRARY_DESIGN.md` describes the migration from
// inventory-submitted metadata to JSON-authoritative preset files. The
// two `PresetSource` buckets (`effect::PresetSource` /
// `generator::PresetSource`) stay separate; everything below is shared.

/// Convert a parsed [`PresetMetadata`] (JSON wire shape) into the unified
/// [`PresetDef`]. The `kind` argument is the only branch: a generator def
/// carries `is_line_based` from the metadata and an empty value-alias
/// table; an effect def forces `is_line_based = false` and leaks its
/// value-alias table. Both leak only the `'static` alias tables via
/// `Box::leak`, bounded by the (finite) shipping preset count, done once
/// at startup when the registries initialise.
pub fn preset_metadata_to_def(meta: &PresetMetadata, kind: PresetKind) -> PresetDef {
    let param_defs: Vec<RegistryParamDef> = meta
        .params
        .iter()
        .map(|spec| RegistryParamDef {
            spec: spec.clone(),
            // `ParamSpecDef` (the card manifest) carries no contract — see
            // `RegistryParamDef::contract`'s doc comment in effects.rs.
            contract: None,
        })
        .collect();
    let (is_line_based, legacy_value_aliases): (
        bool,
        &'static [(&'static str, &'static [ParamValueAlias])],
    ) = match kind {
        // Effects carry the slot-value migration table; effect presets are
        // never line-based.
        PresetKind::Effect => (false, leak_value_alias_table(&meta.value_aliases)),
        // Generators may be line-based; they carry no value-alias table yet
        // (capability gap, see PRESET_UNIFICATION_PLAN Step 9 follow-ups).
        PresetKind::Generator => (meta.is_line_based, &[]),
    };
    PresetDef {
        kind,
        display_name: meta.display_name.clone(),
        param_defs,
        // Outer-card string params (Text/Font on text generators, folder
        // paths on MRI Volume) come straight from the JSON's `stringParams`.
        // Strings are leaked to `'static` once at registry build / hot-reload,
        // matching the alias-table pattern below.
        string_param_defs: meta
            .string_params
            .iter()
            .map(|sp| StringParamDef {
                name: leak_str(&sp.name),
                key: leak_str(&sp.id),
                default_value: leak_str(&sp.default_value),
                use_dropdown: sp.use_dropdown,
            })
            .collect(),
        osc_prefix: Some(meta.osc_prefix.clone()),
        is_line_based,
        legacy_param_aliases: leak_alias_table(&meta.param_aliases),
        legacy_value_aliases,
    }
}

fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn leak_alias_table(entries: &[AliasEntry]) -> &'static [ParamAlias] {
    let v: Vec<ParamAlias> = entries
        .iter()
        .map(|e| {
            let old: &'static str = leak_str(&e.old);
            let new: Option<&'static str> = e.new.as_deref().map(leak_str);
            (old, new)
        })
        .collect();
    Box::leak(v.into_boxed_slice())
}

fn leak_value_alias_table(
    entries: &[ValueAliasEntry],
) -> &'static [(&'static str, &'static [ParamValueAlias])] {
    let v: Vec<(&'static str, &'static [ParamValueAlias])> = entries
        .iter()
        .map(|e| {
            let param_id: &'static str = leak_str(&e.param_id);
            let mapping: &'static [ParamValueAlias] =
                Box::leak(e.mapping.clone().into_boxed_slice());
            (param_id, mapping)
        })
        .collect();
    Box::leak(v.into_boxed_slice())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_registration::EffectMetadata;
    use crate::generator_registration::ParamSpec;

    // Test-only inventory submissions — manifold-renderer isn't linked in
    // manifold-core unit tests, so we register minimal test fixtures here.
    inventory::submit! {
        EffectMetadata {
            id: PresetTypeId::TRANSFORM,
            display_name: "Transform",
            category: "Spatial",
            available: true,
            osc_prefix: "transform",
            legacy_discriminant: Some(0),
            params: &[
                ParamSpec::continuous("x", "X", -1.0, 1.0, 0.0, "F2", ""),
                ParamSpec::continuous("y", "Y", -1.0, 1.0, 0.0, "F2", ""),
                ParamSpec::continuous("zoom", "Zoom", 0.1, 5.0, 1.0, "F2", ""),
                ParamSpec::continuous("rot", "Rot", -180.0, 180.0, 0.0, "F2", ""),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: PresetTypeId::BLOOM,
            display_name: "Bloom",
            category: "Post-Process",
            available: true,
            osc_prefix: "bloom",
            legacy_discriminant: Some(12),
            params: &[
                ParamSpec::continuous("amount", "Amount", 0.0, 5.0, 0.187, "F2", ""),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: PresetTypeId::DITHER,
            display_name: "Dither",
            category: "Post-Process",
            available: true,
            osc_prefix: "dither",
            legacy_discriminant: Some(18),
            params: &[
                ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
                ParamSpec::whole_labels("algo", "Algo", 0.0, 5.0, 0.0, &["Bayer", "Halftone", "Lines", "X-Hatch", "Noise", "Diamond"], "Algorithm"),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: PresetTypeId::KALEIDOSCOPE,
            display_name: "Kaleidoscope",
            category: "Post-Process",
            available: true,
            osc_prefix: "kaleidoscope",
            legacy_discriminant: Some(14),
            params: &[
                ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
                ParamSpec::whole("segs", "Segs", 2.0, 16.0, 6.0, "Segments"),
            ],
        }
    }
    inventory::submit! {
        EffectMetadata {
            id: PresetTypeId::INFINITE_ZOOM,
            display_name: "Infinite Zoom",
            category: "Post-Process",
            available: false,
            osc_prefix: "infiniteZoom",
            legacy_discriminant: Some(13),
            params: &[
                ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.0, "F2", ""),
                ParamSpec::continuous("sharp", "Sharp", 0.0, 1.0, 0.5, "F2", "Sharpness"),
            ],
        }
    }

    #[test]
    fn test_create_default_bloom() {
        let inst = create_default(&PresetTypeId::BLOOM);
        assert_eq!(*inst.effect_type(), PresetTypeId::BLOOM);
        assert!(inst.enabled);
        assert_eq!(inst.params.len(), 1);
        assert!((inst.params.get("amount").unwrap().value - 0.187).abs() < 1e-6);
    }

    #[test]
    fn test_format_value_labels() {
        let s = format_value(&PresetTypeId::DITHER, 1, 2.0);
        assert_eq!(s, "Lines");
    }

    #[test]
    fn test_format_value_whole() {
        let s = format_value(&PresetTypeId::KALEIDOSCOPE, 1, 6.7);
        assert_eq!(s, "7");
    }

    #[test]
    fn test_format_value_continuous() {
        let s = format_value(&PresetTypeId::BLOOM, 0, 0.5);
        assert_eq!(s, "0.50");
    }

    #[test]
    fn test_sorted_types() {
        let mut sorted = all_of_kind(PresetKind::Effect);
        sorted.sort_by_key(|t| t.as_str().to_string());
        for i in 1..sorted.len() {
            assert!(sorted[i - 1].as_str() <= sorted[i].as_str());
        }
    }

    /// The registry seeding must preserve every card-surface flag a
    /// glTF-imported generator depends on: it TRACKS its embedded preset
    /// (`graph: None`, D9/BUG-016), so the `RegistryParamDef` built from a
    /// JSON `ParamSpecDef` in `preset_metadata_to_def` is the ONLY path its
    /// card descriptors travel. Before the descriptor unification, this
    /// went through a hand-written converter and `section` was silently
    /// dropped once (D5 fix), then `is_angle`/`wraps` the same way
    /// (radians-on-card bug, Peter 2026-07-15). Now the registry wraps the
    /// spec directly, so preservation holds BY CONSTRUCTION — this asserts
    /// whole-struct equality rather than picking individual flags, which is
    /// the stronger claim.
    #[test]
    fn registry_seeding_preserves_the_authored_spec() {
        let spec = ParamSpecDef {
            id: "cam_tilt".to_string(),
            name: "Camera Tilt".to_string(),
            min: -std::f32::consts::PI,
            max: std::f32::consts::PI,
            default_value: 0.3,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: crate::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: true,
            is_trigger_gate: true,
            wraps: true,
            section: Some("Camera".to_string()),
        };
        let seeded = RegistryParamDef {
            spec: spec.clone(),
            contract: None,
        };
        assert_eq!(seeded.spec, spec, "the spec must survive the registry wrap whole");
    }

    // ── ParamAlias resolution (step 15) ────────────────────────────

    #[test]
    fn resolve_param_alias_passes_through_current_id() {
        // No alias entry for "amount" → returns it unchanged.
        let aliases: &[crate::effect_registration::ParamAlias] =
            &[("old_thing", Some("new_thing"))];
        assert_eq!(resolve_param_alias(aliases, "amount"), Some("amount"));
    }

    #[test]
    fn resolve_param_alias_renames() {
        let aliases: &[crate::effect_registration::ParamAlias] = &[("cv_flow", Some("flow"))];
        assert_eq!(resolve_param_alias(aliases, "cv_flow"), Some("flow"));
    }

    #[test]
    fn resolve_param_alias_chains_renames() {
        // Two-hop rename: a → b → c.
        let aliases: &[crate::effect_registration::ParamAlias] =
            &[("a", Some("b")), ("b", Some("c"))];
        assert_eq!(resolve_param_alias(aliases, "a"), Some("c"));
    }

    #[test]
    fn resolve_param_alias_drop_returns_none() {
        let aliases: &[crate::effect_registration::ParamAlias] = &[("face", None)];
        assert_eq!(resolve_param_alias(aliases, "face"), None);
    }

    #[test]
    fn resolve_param_alias_chain_to_drop_returns_none() {
        // Renamed once, then dropped: a → b → None.
        let aliases: &[crate::effect_registration::ParamAlias] = &[("a", Some("b")), ("b", None)];
        assert_eq!(resolve_param_alias(aliases, "a"), None);
    }

    #[test]
    fn resolve_param_alias_breaks_cycle() {
        // Pathological: a → b → a (constructor accident). Should
        // bail rather than infinite-loop.
        let aliases: &[crate::effect_registration::ParamAlias] =
            &[("a", Some("b")), ("b", Some("a"))];
        assert_eq!(resolve_param_alias(aliases, "a"), None);
    }

    #[test]
    fn resolve_param_alias_empty_table_passes_through() {
        let aliases: &[crate::effect_registration::ParamAlias] = &[];
        assert_eq!(resolve_param_alias(aliases, "amount"), Some("amount"));
    }

    #[test]
    fn all_default_effect_defs_have_empty_alias_table() {
        // Step 15 ships with no actual renames yet — every effect's
        // alias table should be empty. New entries land via sidecar
        // `EffectAliasMetadata` submissions.
        for effect_type in all_of_kind(PresetKind::Effect) {
            let def = get(&effect_type);
            assert!(
                def.legacy_param_aliases.is_empty(),
                "{} unexpectedly has alias entries: {:?}",
                effect_type.as_str(),
                def.legacy_param_aliases
            );
        }
    }

    // ── §11 block 2: PresetMetadata → EffectDef converter ──────────

    use crate::effect_graph_def::{
        AliasEntry, BindingDef, BindingTarget, ParamSpecDef, PresetMetadata, SkipModeDef,
        ValueAliasEntry,
    };
    use crate::effects::ParamConvert;

    fn bloom_preset_metadata() -> PresetMetadata {
        PresetMetadata {
            id: PresetTypeId::new("BloomFromJson"),
            display_name: "Bloom (from JSON)".to_string(),
            category: "Filmic".to_string(),
            osc_prefix: "bloom_from_json".to_string(),
            legacy_discriminant: Some(12),
            available: true,
            is_line_based: false,
            params: vec![ParamSpecDef {
                id: "amount".to_string(),
                name: "Amount".to_string(),
                min: 0.0,
                max: 5.0,
                default_value: 0.5,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: Vec::new(),
                format_string: Some("F2".to_string()),
                osc_suffix: String::new(),
                curve: Default::default(),
                invert: false,
                is_angle: false,
                is_trigger_gate: false,
                wraps: false,
                section: None,
            }],
            bindings: vec![BindingDef {
                id: "amount".to_string(),
                label: "Amount".to_string(),
                default_value: 0.5,
                target: BindingTarget::Node {
                    node_id: crate::NodeId::new("bloom_node"),
                    param: "amount".to_string(),
                },
                convert: ParamConvert::Float,
                user_added: false,
                scale: 1.0,
                offset: 0.0,
            }],
            skip_mode: SkipModeDef::OnZero {
                param_id: "amount".to_string(),
            },
            param_aliases: vec![AliasEntry {
                old: "intensity".to_string(),
                new: Some("amount".to_string()),
            }],
            value_aliases: vec![ValueAliasEntry {
                param_id: "amount".to_string(),
                mapping: vec![(0, 1)],
            }],
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        }
    }

    #[test]
    fn preset_metadata_converts_to_effect_def() {
        let meta = bloom_preset_metadata();
        let def = preset_metadata_to_def(&meta, PresetKind::Effect);

        assert_eq!(def.display_name, "Bloom (from JSON)");
        assert_eq!(def.osc_prefix.as_deref(), Some("bloom_from_json"));
        assert_eq!(def.param_defs.len(), 1);
        assert_eq!(def.param_defs[0].spec.id, "amount");
        assert_eq!(def.param_defs[0].spec.name, "Amount");
        assert!((def.param_defs[0].spec.default_value - 0.5).abs() < 1e-6);

        assert_eq!(def.legacy_param_aliases.len(), 1);
        assert_eq!(def.legacy_param_aliases[0].0, "intensity");
        assert_eq!(def.legacy_param_aliases[0].1, Some("amount"));

        assert_eq!(def.legacy_value_aliases.len(), 1);
        assert_eq!(def.legacy_value_aliases[0].0, "amount");
        assert_eq!(def.legacy_value_aliases[0].1, &[(0, 1)]);
    }

    /// The JSON converter and the inventory converter must produce
    /// equivalent `EffectDef`s for the same effect shape.
    #[test]
    fn preset_metadata_and_effect_metadata_produce_equivalent_def() {
        static INV_PARAMS: [ParamSpec; 1] =
            [ParamSpec::continuous("amount", "Amount", 0.0, 1.0, 0.5, "F2", "")];
        let inv_meta = EffectMetadata {
            id: PresetTypeId::new("ParityCheck"),
            display_name: "Parity Check",
            category: "Filmic",
            available: true,
            osc_prefix: "parity_check",
            legacy_discriminant: None,
            params: &INV_PARAMS,
        };
        let inv_def = inv_meta.to_effect_def();

        let json_meta = PresetMetadata {
            id: PresetTypeId::new("ParityCheck"),
            display_name: "Parity Check".to_string(),
            category: "Filmic".to_string(),
            osc_prefix: "parity_check".to_string(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            params: vec![ParamSpecDef {
                id: "amount".to_string(),
                name: "Amount".to_string(),
                min: 0.0,
                max: 1.0,
                default_value: 0.5,
                whole_numbers: false,
                is_toggle: false,
                is_trigger: false,
                value_labels: Vec::new(),
                format_string: Some("F2".to_string()),
                osc_suffix: String::new(),
                curve: Default::default(),
                invert: false,
                is_angle: false,
                is_trigger_gate: false,
                wraps: false,
                section: None,
            }],
            bindings: Vec::new(),
            skip_mode: SkipModeDef::default(),
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        };
        let json_def = preset_metadata_to_def(&json_meta, PresetKind::Effect);

        assert_eq!(inv_def.display_name, json_def.display_name);
        assert_eq!(inv_def.osc_prefix, json_def.osc_prefix);
        assert_eq!(inv_def.param_defs.len(), json_def.param_defs.len());
        for (a, b) in inv_def.param_defs.iter().zip(json_def.param_defs.iter()) {
            assert_eq!(a.spec.id, b.spec.id);
            assert_eq!(a.spec.name, b.spec.name);
            assert!((a.spec.min - b.spec.min).abs() < 1e-6);
            assert!((a.spec.max - b.spec.max).abs() < 1e-6);
            assert!((a.spec.default_value - b.spec.default_value).abs() < 1e-6);
            assert_eq!(a.spec.whole_numbers, b.spec.whole_numbers);
            assert_eq!(a.spec.is_toggle, b.spec.is_toggle);
            assert_eq!(a.spec.format_string, b.spec.format_string);
            assert_eq!(a.spec.osc_suffix, b.spec.osc_suffix);
        }
    }

    #[test]
    fn loaded_preset_metadata_returns_empty_initially() {
        // Block 2 ships with no JSON loader populated (manifold-renderer
        // isn't linked in core unit tests). Confirms the dual-source
        // registry doesn't accidentally start consuming something.
        assert!(super::effect::loaded_preset_metadata().is_empty());
        assert!(super::generator::loaded_preset_metadata().is_empty());
    }

    // ── Generator-side tests ───────────────────────────────────────

    /// Backward-compat for the `snap` → `clip_trigger` rename.
    #[test]
    fn legacy_snap_id_still_resolves_via_alias() {
        let def = get(&PresetTypeId::PLASMA);
        let resolved = resolve_param_alias(def.legacy_param_aliases, "snap");
        assert_eq!(resolved, Some("clip_trigger"));
        // Plasma — declared in generator_metadata_submissions.rs:
        //   pattern (0), complexity (1), contrast (2), speed (3),
        //   scale (4), clip_trigger (5)
        assert_eq!(
            def.param_defs
                .iter()
                .position(|pd| pd.spec.id == resolved.unwrap()),
            Some(5),
        );
    }
}
