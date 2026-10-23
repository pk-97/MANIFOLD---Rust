//! Immutable catalog assets shared by integration proofs.

pub const CATALOG_ASSETS_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");
pub const ASSETS_GENERATOR_PRESETS_PHYSICSSOLIDS_JSON: &str = include_str!("../../tests/fixtures/presets/PhysicsSolids.json");
pub const ASSETS_GENERATOR_PRESETS_WATERDAMBREAKGPUFLIP_JSON: &str = include_str!("../../tests/fixtures/presets/WaterDamBreakGpuFlip.json");
pub const ASSETS_GENERATOR_PRESETS_WATERDAMBREAKMATTER_JSON: &str = include_str!("../../tests/fixtures/presets/WaterDamBreakMatter.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_ELASTICSCULPTURE_JSON: &str = include_str!("../../assets/scene-modifier-presets/ElasticSculpture.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_MATHVIEW_JSON: &str = include_str!("../../assets/scene-modifier-presets/MathView.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_ORDEREDRECON_JSON: &str = include_str!("../../assets/scene-modifier-presets/OrderedRecon.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_RADIALFORCE_JSON: &str = include_str!("../../assets/scene-modifier-presets/RadialForce.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_SCENEFOG_JSON: &str = include_str!("../../assets/scene-modifier-presets/SceneFog.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_SCENELOOP_JSON: &str = include_str!("../../assets/scene-modifier-presets/SceneLoop.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_SHATTER_JSON: &str = include_str!("../../assets/scene-modifier-presets/Shatter.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_SPATIALECHOES_JSON: &str = include_str!("../../assets/scene-modifier-presets/SpatialEchoes.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_SURFACEPEEL_JSON: &str = include_str!("../../assets/scene-modifier-presets/SurfacePeel.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_SURFACEWAVES_JSON: &str = include_str!("../../assets/scene-modifier-presets/SurfaceWaves.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_UNIFORMFORCE_JSON: &str = include_str!("../../assets/scene-modifier-presets/UniformForce.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_VORTEXFORCE_JSON: &str = include_str!("../../assets/scene-modifier-presets/VortexForce.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_VORTEXFRAGMENTS_JSON: &str = include_str!("../../assets/scene-modifier-presets/VortexFragments.json");
pub const ASSETS_SCENE_MODIFIER_PRESETS_WIND_JSON: &str = include_str!("../../assets/scene-modifier-presets/Wind.json");
pub const TESTS_FIXTURES_SCENE_MODIFIERS_NESTED_MULTIMATERIAL_V2_JSON: &str = include_str!("../../tests/fixtures/scene-modifiers/nested_multimaterial_v2.json");
