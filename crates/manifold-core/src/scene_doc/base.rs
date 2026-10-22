//! The base scene (SCENE_DOCUMENT_DESIGN.md section 7.5 (The base scene)):
//! what every new scene holds, as Blender's startup file does. Its values
//! (camera framing, sun placement, gravity) are the kind tables' defaults.

use super::{
    CameraFields, EnvironmentSource, Item, ItemFields, ItemId, LightFields, LightType, ObjectFields, SceneDoc,
    Shape, SkyStyle, WorldFields,
};

impl SceneDoc {
    /// Camera, Sun, Environment (Softbox sky), World, and one cube at the
    /// origin with physics None.
    pub fn base() -> Self {
        let item = |id: &str, name: &str, fields| Item::new(ItemId::new(id), name, fields);
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
                item("cube", "Cube", ItemFields::Object(ObjectFields { shape: Shape::Cube, ..ObjectFields::default() })),
            ],
        }
    }
}
