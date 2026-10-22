//! The base scene (SCENE_DOCUMENT_DESIGN.md section 7.5 (The base scene)):
//! what every new scene holds, as Blender's startup file does. Its values
//! (camera framing, sun placement, gravity) are the kind tables' defaults.

use super::{
    CameraFields, EnvironmentSource, Item, ItemFields, ItemId, LightFields, LightType, MaterialFields, ObjectFields,
    SceneDoc, Shape, SkyStyle, WorldFields,
};

/// The base scene's cube and the Material it draws with.
pub const BASE_CUBE: &str = "cube";
pub const BASE_CUBE_MATERIAL: &str = "cube-material";

impl SceneDoc {
    /// Camera, Sun, Environment (Softbox sky), World, and one cube at the
    /// origin with physics None and its own Material, as Blender's default
    /// cube has.
    pub fn base() -> Self {
        let item = |id: &str, name: &str, fields| Item::new(ItemId::new(id), name, fields);
        let cube = ObjectFields { shape: Shape::Cube, material: Some(ItemId::new(BASE_CUBE_MATERIAL)), ..ObjectFields::default() };
        SceneDoc {
            items: vec![
                item("camera", "Camera", ItemFields::Camera(CameraFields::default())),
                item("sun", "Sun", ItemFields::Light(LightFields { light: LightType::Sun })),
                item(
                    "environment",
                    "Environment",
                    ItemFields::Light(LightFields {
                        light: LightType::Environment { source: EnvironmentSource::Sky { style: SkyStyle::Softbox } },
                    }),
                ),
                item("world", "World", ItemFields::World(WorldFields::default())),
                item(BASE_CUBE, "Cube", ItemFields::Object(cube)),
                item(BASE_CUBE_MATERIAL, "Material", ItemFields::Material(MaterialFields::default())),
            ],
        }
    }
}
