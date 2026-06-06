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

use ahash::AHashMap;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, OnceLock};

use arc_swap::ArcSwap;

use crate::effect_graph_def::{
    AliasEntry, BindingDef, ParamSpecDef, PresetMetadata, SkipModeDef, ValueAliasEntry,
};
use crate::effect_registration::{ParamAlias, ParamValueAlias};
use crate::preset_type_id::PresetTypeId;
use crate::effects::ParamDef;
use crate::preset_def::{PresetDef, PresetKind};

// ─── StringParamDef ───
//
// Generator-only state carried on `PresetDef::string_param_defs`. Lived
// in the old `generator_definition_registry`; moved here with the merge.

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

// ─── Static registries ───

// ─── Static registries (hot-reloadable, step 10) ───
//
// The two stores used to be `LazyLock<HashMap<_, PresetDef>>` borrowed for
// the process lifetime. They are now `ArcSwap` of `Arc<HashMap<_,
// Arc<PresetDef>>>` so the hot-reload watcher can swap in a freshly-built
// map (from the reloaded preset JSON) without a restart via
// [`rebuild_effect_definitions`] / [`rebuild_generator_definitions`].
//
// `get`/`try_get` return a cheap `Arc<PresetDef>` refcount clone. They are
// NOT a per-frame hot path (modulation/Ableton/OSC route through
// `param_id_to_index`, an `AHashMap::get` on the snapshot — see below), so
// the extra atomic pointer load + refcount bump per call is negligible and
// only happens at chain (re)build / editor / addressing setup time.
//
// At rest the stores are never swapped, so a `load_full` returns the same
// `Arc` every time — byte-identical behaviour to the old `LazyLock`.

type EffectMap = HashMap<PresetTypeId, Arc<PresetDef>>;
type GeneratorMap = HashMap<PresetTypeId, Arc<PresetDef>>;

static EFFECT_DEFINITIONS: LazyLock<ArcSwap<EffectMap>> =
    LazyLock::new(|| ArcSwap::from_pointee(build_effect_definitions(effect::loaded_preset_metadata())));

static GENERATOR_DEFINITIONS: LazyLock<ArcSwap<GeneratorMap>> = LazyLock::new(|| {
    ArcSwap::from_pointee(build_generator_definitions(generator::loaded_preset_metadata()))
});

/// `param_id_to_index`'s hot read happens against a snapshot of the effect
/// map. To keep that read a single atomic pointer load (no per-call
/// `load_full` refcount churn on the per-frame addressing path), callers
/// take a snapshot via `EFFECT_DEFINITIONS.load()` and index it.
///
/// Build the effect definition map from the inventory submissions plus the
/// supplied JSON-loaded preset metadata. Shared by the `LazyLock`
/// initializer (passing the cached `loaded_preset_metadata`) and
/// [`rebuild_effect_definitions`] (passing freshly-reloaded metadata).
fn build_effect_definitions(json_presets: &[PresetMetadata]) -> EffectMap {
    let mut m: EffectMap = HashMap::new();
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

/// Build the generator definition map. Mirror of
/// [`build_effect_definitions`] over the generator side.
fn build_generator_definitions(json_presets: &[PresetMetadata]) -> GeneratorMap {
    let mut m: GeneratorMap = HashMap::new();

    // ── None ──
    m.insert(
        PresetTypeId::NONE,
        Arc::new(PresetDef {
            kind: PresetKind::Generator,
            display_name: "None".to_string(),
            is_line_based: false,
            param_count: 0,
            param_defs: Vec::new(),
            string_param_defs: Vec::new(),
            osc_prefix: None,
            id_to_index: AHashMap::new(),
            param_ids: Vec::new(),
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

/// Hot-reload (step 10): rebuild the effect definition map from
/// freshly-reloaded JSON preset metadata and swap it in. Called by the
/// watcher thread **after** the renderer's preset catalog snapshot has been
/// reloaded; the caller passes the new metadata (sourced from the reloaded
/// catalog) so the core stays decoupled from the renderer's disk loader.
///
/// Crash-safe by construction: the new map is built fully before the swap;
/// a swap is atomic, so a concurrent reader never sees a half-built map.
pub fn rebuild_effect_definitions(json_presets: &[PresetMetadata]) {
    EFFECT_DEFINITIONS.store(Arc::new(build_effect_definitions(json_presets)));
}

/// Hot-reload (step 10): rebuild the generator definition map. Mirror of
/// [`rebuild_effect_definitions`]. Also recomputes the
/// `MAX_GEN_PARAM_COUNT` cache so `max_param_count()` reflects the reload.
pub fn rebuild_generator_definitions(json_presets: &[PresetMetadata]) {
    let map = build_generator_definitions(json_presets);
    let max = map.values().map(|d| d.param_count).max().unwrap_or(0);
    MAX_GEN_PARAM_COUNT.store(max as u64, std::sync::atomic::Ordering::Relaxed);
    GENERATOR_DEFINITIONS.store(Arc::new(map));
}

/// Max generator param count, maintained as an atomic so a generator
/// reload can update it without rebuilding a `LazyLock`. Seeded lazily on
/// first access from the current generator map.
static MAX_GEN_PARAM_COUNT: LazyLock<std::sync::atomic::AtomicU64> = LazyLock::new(|| {
    let max = GENERATOR_DEFINITIONS
        .load()
        .values()
        .map(|d| d.param_count)
        .max()
        .unwrap_or(0);
    std::sync::atomic::AtomicU64::new(max as u64)
});

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

// ─── Effect accessors ───
//
// Thin `PresetTypeId`-keyed view over [`EFFECT_DEFINITIONS`]. Function
// names match the legacy `effect_definition_registry` surface exactly —
// only the module path moved.

pub mod effect {
    use super::*;

    /// Re-export for callers within this module's namespace. Canonical
    /// home is [`crate::effect_registration::resolve_param_alias`].
    pub use crate::effect_registration::resolve_param_alias;

    /// Get the definition for an effect type. Panics if not found.
    ///
    /// Returns an owned `Arc<PresetDef>` (cheap refcount clone of the
    /// current registry snapshot). Hot-reload (step 10): a reload swaps the
    /// snapshot, so holding the `Arc` keeps the def the caller looked up
    /// alive even across a concurrent rebuild. Not a per-frame hot path.
    pub fn get(effect_type: &PresetTypeId) -> Arc<PresetDef> {
        try_get(effect_type).unwrap_or_else(|| {
            panic!(
                "EffectDefinitionRegistry: unknown PresetTypeId '{}'",
                effect_type
            )
        })
    }

    /// Try to get the definition for an effect type.
    pub fn try_get(effect_type: &PresetTypeId) -> Option<Arc<PresetDef>> {
        EFFECT_DEFINITIONS.load().get(effect_type).cloned()
    }

    /// Translate a stable `ParamSpec::id` into the param's storage index
    /// for the given effect type. Returns `None` if the effect or id is
    /// unknown.
    ///
    /// Hot-path: every per-frame addressing dispatch (driver, envelope,
    /// Ableton update, OSC route) goes through this. The lookup is one
    /// `&str → usize` `AHashMap::get` (~50ns); the map is built once when
    /// the registry initializes.
    pub fn param_id_to_index(effect_type: &PresetTypeId, id: &str) -> Option<usize> {
        EFFECT_DEFINITIONS
            .load()
            .get(effect_type)?
            .id_to_index
            .get(id)
            .copied()
    }

    /// Reverse of [`param_id_to_index`]: storage index → param id. Returns
    /// an owned `Arc<str>` cloned from the registry snapshot (the old
    /// `&'static str` return is no longer possible now the registry is
    /// hot-reloadable — a swap would dangle a borrowed reference). Returns
    /// `None` if the effect or index is out of range, or the slot has an
    /// empty id (V1 fixture / pre-step-6 entry).
    pub fn param_index_to_id(effect_type: &PresetTypeId, index: usize) -> Option<Arc<str>> {
        let snapshot = EFFECT_DEFINITIONS.load();
        let def = snapshot.get(effect_type)?;
        let id = def.param_ids.get(index)?;
        if id.is_empty() {
            None
        } else {
            Some(Arc::from(id.as_str()))
        }
    }

    /// Create a new PresetInstance with default parameter values from the
    /// registry.
    pub fn create_default(effect_type: &PresetTypeId) -> crate::effects::PresetInstance {
        let def = get(effect_type);
        let mut inst = crate::effects::PresetInstance::new(effect_type.clone());
        for (i, pd) in def.param_defs.iter().enumerate() {
            inst.set_base_param(i, pd.default_value);
        }
        inst
    }

    /// Format a parameter value for display. Named labels take priority,
    /// then wholeNumbers round, then F2.
    pub fn format_value(effect_type: &PresetTypeId, param_index: usize, value: f32) -> String {
        let def = match try_get(effect_type) {
            Some(d) if param_index < d.param_count => d,
            _ => return format!("{:.2}", value),
        };
        let pd = &def.param_defs[param_index];
        if let Some(ref labels) = pd.value_labels {
            let idx = (value.round() as i32).clamp(0, labels.len() as i32 - 1) as usize;
            return labels[idx].clone();
        }
        if pd.whole_numbers {
            return format!("{}", value.round() as i32);
        }
        format!("{:.2}", value)
    }

    /// Get the OSC address for a master effect parameter.
    ///
    /// Unified scheme (preset unification, 2026-05):
    /// `/master/{prefix}/{param_id}` — slash-separated path segments,
    /// stable `param_id` as the leaf. Generators share the identical shape
    /// (minus `/master`), so external senders address effects and
    /// generators with one convention. Returns `None` if the effect has no
    /// OSC prefix or the slot has no stable id.
    pub fn get_osc_address(effect_type: &PresetTypeId, param_index: usize) -> Option<String> {
        let def = try_get(effect_type)?;
        let prefix = def.osc_prefix.as_deref()?;
        let param_id = def.param_ids.get(param_index)?;
        if param_id.is_empty() {
            return None;
        }
        Some(format!("/master/{}/{}", prefix, param_id))
    }

    /// Get the OSC address for a layer effect parameter scoped to a
    /// specific layer. Unified scheme: `/layer/{layerId}/{prefix}/{param_id}`.
    pub fn get_osc_address_for_layer(
        effect_type: &PresetTypeId,
        layer_id: &str,
        param_index: usize,
    ) -> Option<String> {
        if layer_id.is_empty() {
            return None;
        }
        let def = try_get(effect_type)?;
        let prefix = def.osc_prefix.as_deref()?;
        let param_id = def.param_ids.get(param_index)?;
        if param_id.is_empty() {
            return None;
        }
        Some(format!("/layer/{}/{}/{}", layer_id, prefix, param_id))
    }

    /// Get default parameter values for an effect type as freshly-allocated
    /// `ParamSlot` entries, all `exposed: true`.
    pub fn get_defaults(effect_type: &PresetTypeId) -> Vec<crate::effects::ParamSlot> {
        let def = get(effect_type);
        def.param_defs
            .iter()
            .map(|p| crate::effects::ParamSlot::exposed(p.default_value))
            .collect()
    }

    /// Get all registered effect types (unordered).
    pub fn get_all_effect_types() -> Vec<PresetTypeId> {
        EFFECT_DEFINITIONS.load().keys().cloned().collect()
    }

    /// Get all registered effect types sorted by display name.
    pub fn get_all_effect_types_sorted() -> Vec<PresetTypeId> {
        let mut list: Vec<PresetTypeId> = EFFECT_DEFINITIONS.load().keys().cloned().collect();
        list.sort_by_key(|t| t.as_str().to_string());
        list
    }

    /// JSON-loaded **effect** preset metadata for the [`EFFECT_DEFINITIONS`]
    /// registry. Each [`PresetSource`] submission contributes a function
    /// pointer producing a `Vec<PresetMetadata>`. The renderer submits one
    /// source pointing at `loaded_presets_from_bundled` (effect preset
    /// JSON). Sources are invoked once on first access and cached for the
    /// process lifetime.
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

    /// Convert a parsed [`PresetMetadata`] into the picker-side
    /// [`crate::effect_type_registry::EffectTypeRegistration`].
    pub fn preset_metadata_to_type_registration(
        meta: &PresetMetadata,
    ) -> crate::effect_type_registry::EffectTypeRegistration {
        crate::effect_type_registry::EffectTypeRegistration {
            id: meta.id.clone(),
            display_name: Box::leak(meta.display_name.clone().into_boxed_str()),
            category: Box::leak(meta.category.clone().into_boxed_str()),
            available: meta.available,
        }
    }

    /// Convert a parsed [`PresetMetadata`] into a `PresetDef` (kind =
    /// `Effect`). Thin wrapper over [`super::preset_metadata_to_def`] kept
    /// for call-site name-stability with the legacy
    /// `preset_metadata_to_effect_def`.
    pub fn preset_metadata_to_effect_def(meta: &PresetMetadata) -> PresetDef {
        super::preset_metadata_to_def(meta, PresetKind::Effect)
    }
}

// ─── Generator accessors ───
//
// Thin `PresetTypeId`-keyed view over [`GENERATOR_DEFINITIONS`].
// Function names match the legacy `generator_definition_registry` surface
// exactly — only the module path moved.

pub mod generator {
    use super::*;

    pub fn get(gen_type: &PresetTypeId) -> Arc<PresetDef> {
        try_get(gen_type).unwrap_or_else(|| {
            panic!(
                "GeneratorDefinitionRegistry: unknown PresetTypeId '{}'",
                gen_type
            )
        })
    }

    pub fn try_get(gen_type: &PresetTypeId) -> Option<Arc<PresetDef>> {
        GENERATOR_DEFINITIONS.load().get(gen_type).cloned()
    }

    /// Translate a stable `ParamSpec::id` into the param's storage index
    /// for the given generator type. Returns `None` if the generator or id
    /// is unknown. Mirrors [`super::effect::param_id_to_index`].
    pub fn param_id_to_index(gen_type: &PresetTypeId, id: &str) -> Option<usize> {
        GENERATOR_DEFINITIONS
            .load()
            .get(gen_type)?
            .id_to_index
            .get(id)
            .copied()
    }

    /// Reverse of [`param_id_to_index`]. Returns an owned `Arc<str>` cloned
    /// from the registry snapshot (the registry is hot-reloadable, so a
    /// borrowed `&'static str` could dangle across a reload). `None` if out
    /// of range or the slot has an empty id (V1 fixture / pre-step-6 entry).
    pub fn param_index_to_id(gen_type: &PresetTypeId, index: usize) -> Option<Arc<str>> {
        let snapshot = GENERATOR_DEFINITIONS.load();
        let def = snapshot.get(gen_type)?;
        let id = def.param_ids.get(index)?;
        if id.is_empty() {
            None
        } else {
            Some(Arc::from(id.as_str()))
        }
    }

    pub fn is_line_based(gen_type: &PresetTypeId) -> bool {
        GENERATOR_DEFINITIONS
            .load()
            .get(gen_type)
            .is_some_and(|d| d.is_line_based)
    }

    pub fn get_param_def(gen_type: &PresetTypeId, index: usize) -> ParamDef {
        let snapshot = GENERATOR_DEFINITIONS.load();
        let Some(def) = snapshot.get(gen_type) else {
            return ParamDef::default();
        };
        if index >= def.param_count {
            return ParamDef::default();
        }
        def.param_defs[index].clone()
    }

    pub fn get_defaults(gen_type: &PresetTypeId) -> Vec<f32> {
        let snapshot = GENERATOR_DEFINITIONS.load();
        let Some(def) = snapshot.get(gen_type) else {
            return Vec::new();
        };
        def.param_defs.iter().map(|p| p.default_value).collect()
    }

    pub fn format_gen_value(gen_type: &PresetTypeId, index: usize, value: f32) -> String {
        let pd = get_param_def(gen_type, index);

        // Labels take priority
        if let Some(ref labels) = pd.value_labels {
            let idx = (value.round() as i32).clamp(0, labels.len() as i32 - 1) as usize;
            return labels[idx].clone();
        }

        // Whole numbers next
        if pd.whole_numbers {
            return format!("{}", value.round() as i32);
        }

        // Format string next
        if let Some(ref fmt) = pd.format_string {
            return format_float_with_format_string(value, fmt);
        }

        // Default: F2
        format!("{:.2}", value)
    }

    pub fn get_osc_address(gen_type: &PresetTypeId, index: usize) -> Option<String> {
        let snapshot = GENERATOR_DEFINITIONS.load();
        let def = snapshot.get(gen_type)?;
        let prefix = def.osc_prefix.as_deref()?;
        let param_id = def.param_ids.get(index)?;
        if param_id.is_empty() {
            return None;
        }
        Some(format!("/{}/{}", prefix, param_id))
    }

    /// Unified with the effect scheme (preset unification, 2026-05):
    /// `/layer/{layerId}/{prefix}/{param_id}`. The legacy `/gen/` namespace
    /// segment is dropped — disambiguation between an effect and a
    /// generator sharing a layer is a naming-convention concern (distinct
    /// osc_prefixes), not an addressing one.
    pub fn get_osc_address_for_layer(
        gen_type: &PresetTypeId,
        layer_id: &str,
        index: usize,
    ) -> Option<String> {
        if layer_id.is_empty() {
            return None;
        }
        let snapshot = GENERATOR_DEFINITIONS.load();
        let def = snapshot.get(gen_type)?;
        let prefix = def.osc_prefix.as_deref()?;
        let param_id = def.param_ids.get(index)?;
        if param_id.is_empty() {
            return None;
        }
        Some(format!("/layer/{}/{}/{}", layer_id, prefix, param_id))
    }

    pub fn try_get_gen_param_range(gen_type: &PresetTypeId, index: usize) -> Option<(f32, f32)> {
        let snapshot = GENERATOR_DEFINITIONS.load();
        let def = snapshot.get(gen_type)?;
        if index >= def.param_count {
            return None;
        }
        let pd = &def.param_defs[index];
        Some((pd.min, pd.max))
    }

    pub fn clamp_param(gen_type: &PresetTypeId, index: usize, value: f32) -> f32 {
        let snapshot = GENERATOR_DEFINITIONS.load();
        let Some(def) = snapshot.get(gen_type) else {
            return value;
        };
        if index >= def.param_count {
            return value;
        }
        let pd = &def.param_defs[index];
        value.clamp(pd.min, pd.max)
    }

    pub fn max_param_count() -> usize {
        MAX_GEN_PARAM_COUNT.load(std::sync::atomic::Ordering::Relaxed) as usize
    }

    /// JSON-loaded **generator** preset metadata for the
    /// [`GENERATOR_DEFINITIONS`] registry. Mirror of
    /// [`super::effect::loaded_preset_metadata`] over the generator disk
    /// bucket. The renderer submits one source pointing at
    /// `loaded_generator_presets_from_bundled`.
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

    /// Convert a [`PresetMetadata`] into the picker-side
    /// [`crate::generator_type_registry::GeneratorTypeRegistration`].
    pub fn preset_metadata_to_type_registration(
        meta: &PresetMetadata,
    ) -> crate::generator_type_registry::GeneratorTypeRegistration {
        crate::generator_type_registry::GeneratorTypeRegistration {
            id: PresetTypeId::from_string(meta.id.as_str().to_string()),
            display_name: leak_str(&meta.display_name),
            available: meta.available,
        }
    }

    /// Convert a parsed [`PresetMetadata`] into a `PresetDef` (kind =
    /// `Generator`). Thin wrapper over [`super::preset_metadata_to_def`]
    /// kept for call-site name-stability with the legacy
    /// `preset_metadata_to_generator_def`.
    pub fn preset_metadata_to_generator_def(meta: &PresetMetadata) -> PresetDef {
        super::preset_metadata_to_def(meta, PresetKind::Generator)
    }
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
    let param_defs: Vec<ParamDef> = meta.params.iter().map(param_spec_def_to_param_def).collect();
    let param_count = param_defs.len();
    let id_to_index: AHashMap<String, usize> = meta
        .params
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.id.is_empty())
        .map(|(i, p)| (p.id.clone(), i))
        .collect();
    let param_ids: Vec<String> = meta.params.iter().map(|p| p.id.clone()).collect();
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
        param_count,
        param_defs,
        // String params live outside the v2 PresetMetadata schema for now.
        // Generators that need them (Text, NumberStation, …) keep their
        // inventory submission; the §11 path applies to graph-backed
        // presets without a string-param surface.
        string_param_defs: Vec::new(),
        osc_prefix: Some(meta.osc_prefix.clone()),
        is_line_based,
        id_to_index,
        param_ids,
        legacy_param_aliases: leak_alias_table(&meta.param_aliases),
        legacy_value_aliases,
    }
}

fn param_spec_def_to_param_def(p: &ParamSpecDef) -> ParamDef {
    ParamDef {
        id: p.id.clone(),
        name: p.name.clone(),
        min: p.min,
        max: p.max,
        default_value: p.default_value,
        whole_numbers: p.whole_numbers,
        is_toggle: p.is_toggle,
        is_trigger: p.is_trigger,
        value_labels: if p.value_labels.is_empty() {
            None
        } else {
            Some(p.value_labels.clone())
        },
        format_string: p.format_string.clone(),
        osc_suffix: if p.osc_suffix.is_empty() {
            None
        } else {
            Some(p.osc_suffix.clone())
        },
        curve: p.curve,
        invert: p.invert,
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

// Silence unused-warnings for items still in plumbing. The `#[allow]` is
// removed once the items are wired to a non-test consumer.
#[allow(dead_code)]
fn _phase_b_keepalive(_: &BindingDef, _: &SkipModeDef) {}

#[cfg(test)]
mod tests {
    use super::effect::*;
    use super::generator;
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
    fn test_param_counts_match() {
        // Check all registered effects have consistent param counts
        for (_, def) in EFFECT_DEFINITIONS.load().iter() {
            assert_eq!(
                def.param_count,
                def.param_defs.len(),
                "param_count mismatch for {}: declared {} but has {} defs",
                def.display_name,
                def.param_count,
                def.param_defs.len()
            );
        }
    }

    #[test]
    fn test_create_default_bloom() {
        let inst = create_default(&PresetTypeId::BLOOM);
        assert_eq!(*inst.effect_type(), PresetTypeId::BLOOM);
        assert!(inst.enabled);
        assert_eq!(inst.param_values.len(), 1);
        assert!((inst.param_values[0].value - 0.187).abs() < 1e-6);
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
    fn test_osc_address_master() {
        // Unified scheme: /master/{prefix}/{param_id}. Bloom param 0 id = "amount".
        let addr = get_osc_address(&PresetTypeId::BLOOM, 0);
        assert_eq!(addr, Some("/master/bloom/amount".to_string()));
    }

    #[test]
    fn test_osc_address_master_param() {
        // InfiniteZoom param 1 id = "sharp" (slash-separated, stable id leaf —
        // not the legacy concat "/master/infiniteZoomSharpness").
        let addr = get_osc_address(&PresetTypeId::INFINITE_ZOOM, 1);
        assert_eq!(addr, Some("/master/infiniteZoom/sharp".to_string()));
    }

    #[test]
    fn test_osc_address_uniform_for_param_zero_and_beyond() {
        // Every param with a stable id gets an address now — no param-0
        // special case, no "no suffix → None". Transform ids: x, y, zoom, rot.
        assert_eq!(
            get_osc_address(&PresetTypeId::TRANSFORM, 0),
            Some("/master/transform/x".to_string())
        );
        assert_eq!(
            get_osc_address(&PresetTypeId::TRANSFORM, 1),
            Some("/master/transform/y".to_string())
        );
        // Out-of-range index still returns None.
        assert_eq!(get_osc_address(&PresetTypeId::TRANSFORM, 99), None);
    }

    #[test]
    fn test_osc_address_layer() {
        let addr = get_osc_address_for_layer(&PresetTypeId::BLOOM, "layer_1", 0);
        assert_eq!(addr, Some("/layer/layer_1/bloom/amount".to_string()));
    }

    #[test]
    fn test_sorted_types() {
        let sorted = get_all_effect_types_sorted();
        for i in 1..sorted.len() {
            assert!(sorted[i - 1].as_str() <= sorted[i].as_str());
        }
    }

    #[test]
    fn param_id_to_index_resolves_known_ids() {
        // Bloom: single param with id "amount".
        assert_eq!(
            param_id_to_index(&PresetTypeId::BLOOM, "amount"),
            Some(0),
            "bloom.amount must resolve to slot 0"
        );

        // Transform: 4 params in registration order (x, y, zoom, rot).
        assert_eq!(param_id_to_index(&PresetTypeId::TRANSFORM, "x"), Some(0));
        assert_eq!(param_id_to_index(&PresetTypeId::TRANSFORM, "y"), Some(1));
        assert_eq!(param_id_to_index(&PresetTypeId::TRANSFORM, "zoom"), Some(2));
        assert_eq!(param_id_to_index(&PresetTypeId::TRANSFORM, "rot"), Some(3));
    }

    #[test]
    fn param_id_to_index_unknown_id_returns_none() {
        assert_eq!(
            param_id_to_index(&PresetTypeId::BLOOM, "nope"),
            None,
            "unknown id must return None, not a stale or default index"
        );
    }

    #[test]
    fn param_id_to_index_unknown_effect_returns_none() {
        let phantom = PresetTypeId::from_string("not-a-real-effect-id".to_string());
        assert_eq!(param_id_to_index(&phantom, "amount"), None);
    }

    #[test]
    fn param_index_to_id_round_trips() {
        // For each test-fixture effect, every (id → index) entry must
        // round-trip back through param_index_to_id.
        for effect in [
            PresetTypeId::TRANSFORM,
            PresetTypeId::BLOOM,
            PresetTypeId::DITHER,
            PresetTypeId::KALEIDOSCOPE,
        ] {
            let def = get(&effect);
            for (i, pd) in def.param_defs.iter().enumerate() {
                if pd.id.is_empty() {
                    continue;
                }
                assert_eq!(
                    param_id_to_index(&effect, &pd.id),
                    Some(i),
                    "{}::{} must resolve to {}",
                    effect.as_str(),
                    pd.id,
                    i
                );
                assert_eq!(
                    param_index_to_id(&effect, i).as_deref(),
                    Some(pd.id.as_str()),
                    "{} index {} must reverse to {}",
                    effect.as_str(),
                    i,
                    pd.id
                );
            }
        }
    }

    #[test]
    fn param_id_to_index_keys_match_param_count() {
        // Map size must equal the number of params (no dupes, no empties).
        // This catches accidental collisions when adding new effects.
        for effect_type in get_all_effect_types() {
            let def = get(&effect_type);
            let non_empty_id_count = def.param_defs.iter().filter(|pd| !pd.id.is_empty()).count();
            assert_eq!(
                def.id_to_index.len(),
                non_empty_id_count,
                "{}: id_to_index size mismatch — possible duplicate or empty ids",
                effect_type.as_str()
            );
        }
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
        for effect_type in get_all_effect_types() {
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
        assert_eq!(def.param_count, 1);
        assert_eq!(def.param_defs.len(), 1);
        assert_eq!(def.param_defs[0].id, "amount");
        assert_eq!(def.param_defs[0].name, "Amount");
        assert!((def.param_defs[0].default_value - 0.5).abs() < 1e-6);
        assert_eq!(def.id_to_index.get("amount"), Some(&0));
        assert_eq!(def.param_ids, vec!["amount"]);

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
        assert_eq!(inv_def.param_count, json_def.param_count);
        assert_eq!(inv_def.osc_prefix, json_def.osc_prefix);
        assert_eq!(inv_def.param_defs.len(), json_def.param_defs.len());
        for (a, b) in inv_def.param_defs.iter().zip(json_def.param_defs.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.name, b.name);
            assert!((a.min - b.min).abs() < 1e-6);
            assert!((a.max - b.max).abs() < 1e-6);
            assert!((a.default_value - b.default_value).abs() < 1e-6);
            assert_eq!(a.whole_numbers, b.whole_numbers);
            assert_eq!(a.is_toggle, b.is_toggle);
            assert_eq!(a.format_string, b.format_string);
            assert_eq!(a.osc_suffix, b.osc_suffix);
        }
        assert_eq!(inv_def.id_to_index, json_def.id_to_index);
        assert_eq!(inv_def.param_ids, json_def.param_ids);
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

    #[test]
    fn param_id_to_index_resolves_plasma_ids() {
        // Plasma — declared in generator_metadata_submissions.rs:
        //   pattern (0), complexity (1), contrast (2), speed (3),
        //   scale (4), clip_trigger (5)
        assert_eq!(
            generator::param_id_to_index(&PresetTypeId::PLASMA, "pattern"),
            Some(0)
        );
        assert_eq!(
            generator::param_id_to_index(&PresetTypeId::PLASMA, "complexity"),
            Some(1)
        );
        assert_eq!(
            generator::param_id_to_index(&PresetTypeId::PLASMA, "contrast"),
            Some(2)
        );
        assert_eq!(
            generator::param_id_to_index(&PresetTypeId::PLASMA, "speed"),
            Some(3)
        );
        assert_eq!(
            generator::param_id_to_index(&PresetTypeId::PLASMA, "scale"),
            Some(4)
        );
        assert_eq!(
            generator::param_id_to_index(&PresetTypeId::PLASMA, "clip_trigger"),
            Some(5)
        );
    }

    /// Backward-compat for the `snap` → `clip_trigger` rename.
    #[test]
    fn legacy_snap_id_still_resolves_via_alias() {
        let def = generator::get(&PresetTypeId::PLASMA);
        let resolved = resolve_param_alias(def.legacy_param_aliases, "snap");
        assert_eq!(resolved, Some("clip_trigger"));
        assert_eq!(
            generator::param_id_to_index(&PresetTypeId::PLASMA, resolved.unwrap()),
            Some(5),
        );
    }

    #[test]
    fn gen_param_id_to_index_unknown_id_returns_none() {
        assert_eq!(
            generator::param_id_to_index(&PresetTypeId::PLASMA, "nope"),
            None
        );
    }

    #[test]
    fn gen_param_id_to_index_unknown_generator_returns_none() {
        let phantom = PresetTypeId::from_string("not-a-real-generator-id".to_string());
        assert_eq!(generator::param_id_to_index(&phantom, "pattern"), None);
    }

    #[test]
    fn gen_param_id_to_index_round_trips_for_all_known_generators() {
        // Every registered generator's id_to_index map must round-trip
        // each entry through param_index_to_id.
        let snapshot = GENERATOR_DEFINITIONS.load();
        for (gen_id, def) in snapshot.iter() {
            for (i, pd) in def.param_defs.iter().enumerate() {
                if pd.id.is_empty() {
                    continue;
                }
                assert_eq!(
                    generator::param_id_to_index(gen_id, &pd.id),
                    Some(i),
                    "{}::{} must resolve to {}",
                    gen_id.as_str(),
                    pd.id,
                    i
                );
                assert_eq!(
                    generator::param_index_to_id(gen_id, i).as_deref(),
                    Some(pd.id.as_str()),
                    "{} index {} must reverse to {}",
                    gen_id.as_str(),
                    i,
                    pd.id
                );
            }
            // Map size must equal the number of non-empty ids — no dupes.
            let non_empty = def.param_defs.iter().filter(|pd| !pd.id.is_empty()).count();
            assert_eq!(
                def.id_to_index.len(),
                non_empty,
                "{}: id_to_index size mismatch",
                gen_id.as_str()
            );
        }
    }
}
