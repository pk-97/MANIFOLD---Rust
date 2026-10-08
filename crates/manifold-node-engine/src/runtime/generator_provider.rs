//! Catalog-owned generator construction, resolved once by the compositor.

use std::sync::{Arc, LazyLock};
use manifold_core::{PresetTypeId, effect_graph_def::EffectGraphDef, effects::RelightParams, params::ParamManifest};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use super::PresetRuntime;

pub type CreateGenerator = fn(
    Arc<GpuDevice>,
    GpuTextureFormat,
    &PresetTypeId,
    Option<&EffectGraphDef>,
    u32,
    u32,
    bool,
    Option<&ParamManifest>,
    Option<&RelightParams>,
) -> Option<Box<PresetRuntime>>;

pub struct GeneratorProvider {
    pub prewarm: fn(&Arc<GpuDevice>, GpuTextureFormat),
    pub create: CreateGenerator,
}
inventory::collect!(GeneratorProvider);

fn exactly_one<'a>(mut providers: impl Iterator<Item = &'a GeneratorProvider>) -> &'a GeneratorProvider {
    let provider = providers.next().expect("exactly one generator provider must be linked; found none");
    assert!(providers.next().is_none(), "exactly one generator provider must be linked; found multiple");
    provider
}

pub fn generator_provider() -> &'static GeneratorProvider {
    static PROVIDER: LazyLock<&'static GeneratorProvider> = LazyLock::new(|| {
        exactly_one(inventory::iter::<GeneratorProvider>.into_iter())
    });
    *PROVIDER
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROVIDER: GeneratorProvider = GeneratorProvider {
        prewarm: |_, _| panic!("selection must not prewarm"),
        create: |_, _, _, _, _, _, _, _, _| panic!("selection must not construct"),
    };

    #[test]
    fn provider_selection_does_not_prewarm_or_construct() {
        let provider = PROVIDER;
        assert!(std::ptr::eq(exactly_one([&provider].into_iter()), &provider));
    }

    #[test]
    #[should_panic(expected = "found none")]
    fn missing_provider_fails() {
        exactly_one(std::iter::empty());
    }

    #[test]
    #[should_panic(expected = "found multiple")]
    fn multiple_providers_fail() {
        exactly_one([&PROVIDER, &PROVIDER].into_iter());
    }
}
