//! Reads a MaterialX (`.mtlx`) material file, as the CC0 libraries ambientCG
//! and Poly Haven ship one beside each material's pictures. The file names
//! each picture's role, so nothing is guessed from file names. The scene
//! renderer turns the result into the same material a model file's material
//! becomes; Collect copies the pictures it names.

use roxmltree::{Document, Node};
use std::path::{Path, PathBuf};

/// What a picture feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MapSlot {
    BaseColor,
    Opacity,
    Roughness,
    Metalness,
    Normal,
    Occlusion,
    Emission,
    Height,
}

impl MapSlot {
    fn label(self) -> &'static str {
        match self {
            MapSlot::BaseColor => "base colour",
            MapSlot::Opacity => "opacity",
            MapSlot::Roughness => "roughness",
            MapSlot::Metalness => "metalness",
            MapSlot::Normal => "normal",
            MapSlot::Occlusion => "occlusion",
            MapSlot::Emission => "emission",
            MapSlot::Height => "height",
        }
    }
}

/// One picture the material reads.
#[derive(Debug, Clone, PartialEq)]
pub struct MapFile {
    pub slot: MapSlot,
    /// The path as the file writes it, with any `fileprefix` applied: relative
    /// to the .mtlx's folder unless the file wrote an absolute path.
    pub relative: String,
    /// `relative` resolved against the .mtlx's folder.
    pub path: PathBuf,
}

/// A MaterialX file's material.
#[derive(Debug, Clone, PartialEq)]
pub struct Material {
    /// At most one per slot, in the order found.
    pub maps: Vec<MapFile>,
    /// Multiplies the base colour picture, or is the colour when there is none.
    pub base_color: [f32; 3],
    /// Used when there is no opacity picture.
    pub opacity: f32,
    /// Used when there is no roughness picture.
    pub roughness: f32,
    /// Used when there is no metalness picture.
    pub metalness: f32,
    /// The normal map's strength.
    pub normal_scale: f32,
    /// Multiplies the emission picture, or is the emitted colour when there is none.
    pub emission: [f32; 3],
    /// What the file asks for that this reader cannot honour, one plain line each.
    pub warnings: Vec<String>,
}

impl Material {
    pub fn map(&self, slot: MapSlot) -> Option<&MapFile> {
        self.maps.iter().find(|m| m.slot == slot)
    }
}

/// Read the material of the `.mtlx` file at `path`.
pub fn read(path: &Path) -> Result<Material, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse(&text, path.parent().unwrap_or(Path::new("")))
}

/// Whether `path` names a MaterialX file by its extension.
pub fn is_materialx(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mtlx"))
}

const SHADERS: [&str; 4] = [
    "standard_surface",
    "open_pbr_surface",
    "gltf_pbr",
    "UsdPreviewSurface",
];

/// The input names of one shader family and the defaults the file omits.
struct Spec {
    color: &'static str,
    color_def: [f32; 3],
    opacity: &'static str,
    metal: &'static str,
    metal_def: f32,
    rough: &'static str,
    rough_def: f32,
    normal: &'static str,
    occlusion: Option<&'static str>,
    emission: &'static str,
    emission_def: [f32; 3],
}

fn spec(tag: &str) -> Option<Spec> {
    Some(match tag {
        "standard_surface" => Spec {
            color: "base_color",
            color_def: [1.0; 3],
            opacity: "opacity",
            metal: "metalness",
            metal_def: 0.0,
            rough: "specular_roughness",
            rough_def: 0.2,
            normal: "normal",
            occlusion: None,
            emission: "emission_color",
            emission_def: [1.0; 3],
        },
        "open_pbr_surface" => Spec {
            color: "base_color",
            color_def: [0.8; 3],
            opacity: "geometry_opacity",
            metal: "base_metalness",
            metal_def: 0.0,
            rough: "specular_roughness",
            rough_def: 0.3,
            normal: "geometry_normal",
            occlusion: None,
            emission: "emission_color",
            emission_def: [1.0; 3],
        },
        "gltf_pbr" => Spec {
            color: "base_color",
            color_def: [1.0; 3],
            opacity: "alpha",
            metal: "metallic",
            metal_def: 1.0,
            rough: "roughness",
            rough_def: 1.0,
            normal: "normal",
            occlusion: Some("occlusion"),
            emission: "emissive",
            emission_def: [0.0; 3],
        },
        "UsdPreviewSurface" => Spec {
            color: "diffuseColor",
            color_def: [0.18; 3],
            opacity: "opacity",
            metal: "metallic",
            metal_def: 0.0,
            rough: "roughness",
            rough_def: 0.5,
            normal: "normal",
            occlusion: Some("occlusion"),
            emission: "emissiveColor",
            emission_def: [0.0; 3],
        },
        _ => return None,
    })
}

fn is_tag(n: &Node, tag: &str) -> bool {
    n.is_element() && n.tag_name().name() == tag
}

/// The `input` child of `owner` called `name`.
fn input<'a, 'i>(owner: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    owner
        .children()
        .find(|c| is_tag(c, "input") && c.attribute("name") == Some(name))
}

/// The element child of `scope` called `name`.
fn named<'a, 'i>(scope: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    scope
        .children()
        .find(|c| c.is_element() && c.attribute("name") == Some(name))
}

fn is_connected(input: &Node) -> bool {
    input.has_attribute("nodename") || input.has_attribute("nodegraph")
}

fn floats(text: &str) -> Vec<f32> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(|s| s.parse().ok())
        .collect()
}

fn rgb(v: &[f32], default: [f32; 3]) -> [f32; 3] {
    match v {
        [r, g, b, ..] => [*r, *g, *b],
        [x] => [*x; 3],
        _ => default,
    }
}

struct Reader<'a, 'i> {
    root: Node<'a, 'i>,
    folder: &'a Path,
    maps: Vec<MapFile>,
    warnings: Vec<String>,
    normal_scale: f32,
    read: Vec<&'static str>,
}

impl<'a, 'i> Reader<'a, 'i> {
    /// The node a connected `input` points at.
    fn target(&self, input: Node<'a, 'i>) -> Result<Node<'a, 'i>, String> {
        let name = input.attribute("name").unwrap_or("input");
        let missing = |n: &str| format!("{name} names {n}, which the file does not have");
        if let Some(g) = input.attribute("nodegraph") {
            let graph = self
                .root
                .children()
                .find(|c| is_tag(c, "nodegraph") && c.attribute("name") == Some(g))
                .ok_or_else(|| missing(g))?;
            let mut outputs = graph.children().filter(|c| is_tag(c, "output"));
            let output = match input.attribute("output") {
                Some(o) => outputs.find(|c| c.attribute("name") == Some(o)),
                None => outputs.next().filter(|_| outputs.next().is_none()),
            }
            .ok_or_else(|| missing(input.attribute("output").unwrap_or("an output")))?;
            let node = output.attribute("nodename").ok_or_else(|| missing("a node"))?;
            return named(graph, node).ok_or_else(|| missing(node));
        }
        let node = input.attribute("nodename").ok_or_else(|| missing("a node"))?;
        let scope = input
            .parent()
            .and_then(|owner| owner.parent())
            .ok_or_else(|| missing(node))?;
        named(scope, node).ok_or_else(|| missing(node))
    }

    /// Follow a connected input `link` to the picture it reads for `slot`.
    fn follow(&mut self, link: Node<'a, 'i>, slot: MapSlot) -> Result<(), String> {
        let node = self.target(link)?;
        match node.tag_name().name() {
            "image" | "tiledimage" => self.image(node, slot),
            "normalmap" => {
                if slot == MapSlot::Normal
                    && let Some(s) = input_value(node, "scale")
                {
                    self.normal_scale = s.first().copied().unwrap_or(1.0);
                }
                if let Some(space) = node_text(node, "space")
                    && space != "tangent"
                {
                    self.warnings.push(format!(
                        "the normal map is in {space} space; only tangent space is read"
                    ));
                }
                match input(node, "in") {
                    Some(inner) if is_connected(&inner) => self.follow(inner, slot),
                    _ => Ok(()),
                }
            }
            tag => {
                let name = link.attribute("name").unwrap_or("input");
                self.warnings.push(format!(
                    "{name} is driven by a {tag} node, which isn't read; its default is used"
                ));
                Ok(())
            }
        }
    }

    fn image(&mut self, node: Node<'a, 'i>, slot: MapSlot) -> Result<(), String> {
        let Some(file) = input(node, "file") else { return Ok(()) };
        let Some(value) = file.attribute("value") else { return Ok(()) };
        if self.maps.iter().any(|m| m.slot == slot) {
            return Ok(());
        }
        let prefix = file
            .ancestors()
            .find_map(|a| a.attribute("fileprefix"))
            .unwrap_or("");
        let relative = format!("{prefix}{value}");
        let label = slot.label();
        if node.tag_name().name() == "tiledimage"
            && let Some(t) = input_value(node, "uvtiling")
            && let [u, v, ..] = t[..]
            && (u != 1.0 || v != 1.0)
        {
            self.warnings.push(format!(
                "{label} repeats {u}\u{d7}{v} in the file; tiling is set on the material"
            ));
        }
        let space = file.ancestors().find_map(|a| a.attribute("colorspace"));
        let is_color = matches!(slot, MapSlot::BaseColor | MapSlot::Emission);
        match space {
            Some(cs) if is_color && cs != "srgb_texture" => self
                .warnings
                .push(format!("the {label} picture is in {cs}, not sRGB; its colours may look off")),
            Some("srgb_texture") if !is_color => self
                .warnings
                .push(format!("the {label} picture is marked sRGB, but holds data, not colour")),
            _ => {}
        }
        let path = self.folder.join(&relative);
        self.maps.push(MapFile { slot, relative, path });
        Ok(())
    }

    /// The constant on `shader`'s input `name`; a connected input feeds `slot`.
    fn slot_value(
        &mut self,
        shader: Node<'a, 'i>,
        name: &'static str,
        slot: MapSlot,
    ) -> Result<Option<Vec<f32>>, String> {
        self.read.push(name);
        let Some(inp) = input(shader, name) else { return Ok(None) };
        if is_connected(&inp) {
            self.follow(inp, slot)?;
            return Ok(None);
        }
        Ok(inp.attribute("value").map(floats))
    }

    fn scalar(&mut self, shader: Node<'a, 'i>, name: &'static str, slot: MapSlot, default: f32) -> Result<f32, String> {
        Ok(self
            .slot_value(shader, name, slot)?
            .and_then(|v| v.first().copied())
            .unwrap_or(default))
    }

    /// A weight is a constant only; a connected one reads as 1.
    fn weight(&mut self, shader: Node<'a, 'i>, name: &'static str, default: f32) -> f32 {
        self.read.push(name);
        let Some(inp) = input(shader, name) else { return default };
        if is_connected(&inp) {
            self.warnings
                .push(format!("{name} is driven by a picture or node, which isn't read; 1 is used"));
            return 1.0;
        }
        inp.attribute("value")
            .map(floats)
            .and_then(|v| v.first().copied())
            .unwrap_or(default)
    }
}

fn node_text<'a>(owner: Node<'a, '_>, name: &str) -> Option<&'a str> {
    input(owner, name)?.attribute("value")
}

fn input_value(owner: Node, name: &str) -> Option<Vec<f32>> {
    node_text(owner, name).map(floats)
}

/// Parse MaterialX `text`; picture paths resolve against `folder`.
pub fn parse(text: &str, folder: &Path) -> Result<Material, String> {
    let doc = Document::parse(text).map_err(|e| format!("not a readable MaterialX file: {e}"))?;
    let root = doc.root_element();
    if !is_tag(&root, "materialx") {
        return Err("not a MaterialX file: the top element is not <materialx>".into());
    }
    let mut r = Reader {
        root,
        folder,
        maps: Vec::new(),
        warnings: Vec::new(),
        normal_scale: 1.0,
        read: Vec::new(),
    };

    let materials: Vec<_> = root.descendants().filter(|n| is_tag(n, "surfacematerial")).collect();
    let (shader, displacement) = match materials.first() {
        Some(&mat) => {
            if materials.len() > 1 {
                r.warnings.push(format!(
                    "the file holds {} materials; the first is used",
                    materials.len()
                ));
            }
            let link = input(mat, "surfaceshader")
                .filter(is_connected)
                .ok_or("the material names no surface shader")?;
            (r.target(link)?, input(mat, "displacementshader").filter(is_connected))
        }
        None => {
            let shader = root
                .descendants()
                .find(|n| n.is_element() && SHADERS.contains(&n.tag_name().name()));
            match shader {
                Some(s) => (s, None),
                None => {
                    let found: Vec<_> = root
                        .children()
                        .filter(|n| n.is_element())
                        .map(|n| n.tag_name().name())
                        .collect();
                    return Err(format!(
                        "no surface shader the app reads (it reads {}); the file holds: {}",
                        SHADERS.join(", "),
                        found.join(", ")
                    ));
                }
            }
        }
    };
    let tag = shader.tag_name().name();
    let s = spec(tag).ok_or_else(|| {
        format!("the surface shader is a {tag}; the app reads {}", SHADERS.join(", "))
    })?;

    let base_weight = match tag {
        "standard_surface" => r.weight(shader, "base", 1.0),
        "open_pbr_surface" => r.weight(shader, "base_weight", 1.0),
        _ => 1.0,
    };
    let color = r.slot_value(shader, s.color, MapSlot::BaseColor)?;
    let opacity = r.scalar(shader, s.opacity, MapSlot::Opacity, 1.0)?;
    let metalness = r.scalar(shader, s.metal, MapSlot::Metalness, s.metal_def)?;
    let roughness = r.scalar(shader, s.rough, MapSlot::Roughness, s.rough_def)?;
    r.slot_value(shader, s.normal, MapSlot::Normal)?;
    if let Some(name) = s.occlusion {
        r.slot_value(shader, name, MapSlot::Occlusion)?;
    }
    let emission = r.slot_value(shader, s.emission, MapSlot::Emission)?;
    let emission_weight = match tag {
        "standard_surface" => r.weight(shader, "emission", 0.0),
        "gltf_pbr" => r.weight(shader, "emissive_strength", 1.0),
        "open_pbr_surface" => {
            if r.weight(shader, "emission_luminance", 0.0) > 0.0 {
                r.warnings
                    .push("emission brightness is set on the material, not read from the file".into());
                1.0
            } else {
                0.0
            }
        }
        _ => 1.0,
    };
    if let Some(d) = displacement {
        let node = r.target(d)?;
        if is_tag(&node, "displacement")
            && let Some(inp) = input(node, "displacement").filter(is_connected)
        {
            r.follow(inp, MapSlot::Height)?;
        }
    }
    for inp in shader.children().filter(|c| is_tag(c, "input")) {
        let name = inp.attribute("name").unwrap_or("");
        if is_connected(&inp) && name != "tangent" && !r.read.contains(&name) {
            r.warnings.push(format!("{name} is connected, which isn't read"));
        }
    }

    let has = |slot| r.maps.iter().any(|m| m.slot == slot);
    let base_color = if has(MapSlot::BaseColor) {
        [base_weight; 3]
    } else {
        rgb(&color.unwrap_or_default(), s.color_def).map(|c| c * base_weight)
    };
    let emission = if has(MapSlot::Emission) {
        [emission_weight; 3]
    } else {
        rgb(&emission.unwrap_or_default(), s.emission_def).map(|c| c * emission_weight)
    };
    Ok(Material {
        base_color,
        opacity,
        roughness,
        metalness,
        normal_scale: r.normal_scale,
        emission,
        maps: r.maps,
        warnings: r.warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AMBIENT_CG: &str = r#"<?xml version="1.0"?>
<materialx version="1.39" fileprefix="./">
  <open_pbr_surface xpos="6.159420" type="surfaceshader" name="Concrete034_1K_JPG_OpenPbrSurface" ypos="-1.879310">
    <input type="color3" nodename="Concrete034_1K_JPG_Color" name="base_color" />
    <input type="vector3" nodename="NormalMap" name="geometry_normal" />
    <input type="float" nodename="Concrete034_1K_JPG_Roughness" name="specular_roughness" />
  </open_pbr_surface>
  <surfacematerial xpos="8.695652" type="material" name="Concrete034_1K_JPG" ypos="0.000000">
    <input type="surfaceshader" nodename="Concrete034_1K_JPG_OpenPbrSurface" name="surfaceshader" />
    <input type="displacementshader" nodename="displacement" name="displacementshader" />
  </surfacematerial>
  <tiledimage xpos="3.623188" type="color3" name="Concrete034_1K_JPG_Color" ypos="-3.103448">
    <input type="filename" value="Concrete034_1K-JPG_Color.jpg" name="file" colorspace="srgb_texture" />
    <input type="vector2" value="1.0, 1.0" name="uvtiling" />
  </tiledimage>
  <tiledimage xpos="3.623188" type="float" name="Concrete034_1K_JPG_Displacement" ypos="5.163793">
    <input type="filename" value="Concrete034_1K-JPG_Displacement.jpg" name="file" />
    <input type="vector2" value="1.0, 1.0" name="uvtiling" />
  </tiledimage>
  <displacement xpos="6.159420" type="displacementshader" name="displacement" ypos="1.879310">
    <input type="float" nodename="Concrete034_1K_JPG_Displacement" name="displacement" />
    <input type="float" value="1.0" name="scale" />
  </displacement>
  <tiledimage xpos="1.086957" type="vector3" name="Concrete034_1K_JPG_NormalGL" ypos="0.879310">
    <input type="filename" value="Concrete034_1K-JPG_NormalGL.jpg" name="file" />
    <input type="vector2" value="1.0, 1.0" name="uvtiling" />
  </tiledimage>
  <normalmap xpos="3.623188" type="vector3" name="NormalMap" ypos="3.586207">
    <input type="vector3" nodename="Concrete034_1K_JPG_NormalGL" name="in" />
    <input type="float" value="1.0" name="scale" />
  </normalmap>
  <tiledimage xpos="3.623188" type="float" name="Concrete034_1K_JPG_Roughness" ypos="-0.413793">
    <input type="filename" value="Concrete034_1K-JPG_Roughness.jpg" name="file" />
    <input type="vector2" value="1.0, 1.0" name="uvtiling" />
  </tiledimage>
</materialx>"#;

    const POLY_HAVEN: &str = r#"<?xml version="1.0"?>
<materialx version="1.38">
  <nodegraph name="NG_brick_wall_001">
    <texcoord name="Texcoord" type="vector2">
      <input name="index" type="integer" value="0" />
    </texcoord>
    <image name="diff" type="color3">
      <input name="file" type="filename" value="textures/brick_wall_001_diff_1k.jpg" colorspace="srgb_texture" />
      <input name="default" type="color3" value="0.8, 0.6, 0.4" />
      <input name="texcoord" type="vector2" nodename="Texcoord" />
    </image>
    <image name="rough" type="float">
      <input name="file" type="filename" value="textures/brick_wall_001_rough_1k.exr" />
      <input name="default" type="float" value="0.3" />
      <input name="texcoord" type="vector2" nodename="Texcoord" />
    </image>
    <image name="normal" type="vector3">
      <input name="file" type="filename" value="textures/brick_wall_001_nor_gl_1k.exr" />
      <input name="default" type="vector3" value="0.5, 0.5, 1" />
      <input name="texcoord" type="vector2" nodename="Texcoord" />
    </image>
    <normalmap name="Normalmap" type="vector3">
      <input name="in" type="vector3" nodename="normal" />
      <input name="space" type="string" value="tangent" />
      <input name="scale" type="float" value="1" />
    </normalmap>
    <tangent name="Tangent" type="vector3">
      <input name="space" type="string" value="world" />
      <input name="index" type="integer" value="0" />
    </tangent>
    <image name="disp" type="float">
      <input name="file" type="filename" value="textures/brick_wall_001_disp_1k.png" />
      <input name="default" type="float" value="0" />
      <input name="texcoord" type="vector2" nodename="Texcoord" />
    </image>
    <output name="output_diff_out" type="color3" nodename="diff" />
    <output name="output_rough_out" type="float" nodename="rough" />
    <output name="output_Normalmap_out" type="vector3" nodename="Normalmap" />
    <output name="output_Tangent_out" type="vector3" nodename="Tangent" />
    <output name="output_disp_out" type="float" nodename="disp" />
  </nodegraph>
  <displacement name="Displacement" type="displacementshader">
    <input name="displacement" type="float" output="output_disp_out" nodegraph="NG_brick_wall_001" />
    <input name="scale" type="float" value="0.01" />
  </displacement>
  <standard_surface name="SR_brick_wall_001" type="surfaceshader">
    <input name="base" type="float" value="1" />
    <input name="base_color" type="color3" output="output_diff_out" nodegraph="NG_brick_wall_001" />
    <input name="metalness" type="float" value="0" />
    <input name="specular_roughness" type="float" output="output_rough_out" nodegraph="NG_brick_wall_001" />
    <input name="normal" type="vector3" output="output_Normalmap_out" nodegraph="NG_brick_wall_001" />
    <input name="tangent" type="vector3" output="output_Tangent_out" nodegraph="NG_brick_wall_001" />
    <input name="sheen" type="float" value="0" />
  </standard_surface>
  <surfacematerial name="brick_wall_001" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="SR_brick_wall_001" />
    <input name="displacementshader" type="displacementshader" nodename="Displacement" />
  </surfacematerial>
</materialx>"#;

    fn rel(m: &Material, slot: MapSlot) -> Option<&str> {
        m.map(slot).map(|f| f.relative.as_str())
    }

    #[test]
    fn ambientcg_open_pbr_with_file_prefix() {
        let folder = Path::new("/lib/Concrete034");
        let m = parse(AMBIENT_CG, folder).unwrap();
        assert_eq!(rel(&m, MapSlot::BaseColor), Some("./Concrete034_1K-JPG_Color.jpg"));
        assert_eq!(rel(&m, MapSlot::Normal), Some("./Concrete034_1K-JPG_NormalGL.jpg"));
        assert_eq!(rel(&m, MapSlot::Roughness), Some("./Concrete034_1K-JPG_Roughness.jpg"));
        assert_eq!(rel(&m, MapSlot::Height), Some("./Concrete034_1K-JPG_Displacement.jpg"));
        let color = m.map(MapSlot::BaseColor).unwrap();
        assert_eq!(color.path, folder.join("./Concrete034_1K-JPG_Color.jpg"));
        assert_eq!(m.maps.len(), 4);
        assert_eq!(m.metalness, 0.0);
        assert_eq!(m.base_color, [1.0; 3]);
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    #[test]
    fn poly_haven_standard_surface_through_a_nodegraph() {
        let m = parse(POLY_HAVEN, Path::new("/lib/brick_wall_001")).unwrap();
        assert_eq!(rel(&m, MapSlot::BaseColor), Some("textures/brick_wall_001_diff_1k.jpg"));
        assert_eq!(rel(&m, MapSlot::Roughness), Some("textures/brick_wall_001_rough_1k.exr"));
        assert_eq!(rel(&m, MapSlot::Normal), Some("textures/brick_wall_001_nor_gl_1k.exr"));
        assert_eq!(rel(&m, MapSlot::Height), Some("textures/brick_wall_001_disp_1k.png"));
        assert_eq!(m.metalness, 0.0);
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);
    }

    #[test]
    fn colour_through_a_multiply_node_warns_and_keeps_the_default() {
        let text = r#"<materialx>
  <multiply name="tint" type="color3">
    <input name="in1" type="color3" value="1, 0, 0" />
  </multiply>
  <standard_surface name="S" type="surfaceshader">
    <input name="base_color" type="color3" nodename="tint" />
  </standard_surface>
</materialx>"#;
        let m = parse(text, Path::new("/lib")).unwrap();
        assert!(m.map(MapSlot::BaseColor).is_none());
        assert_eq!(m.warnings.len(), 1);
        assert!(m.warnings[0].contains("multiply"));
        assert_eq!(m.base_color, [1.0; 3]);
    }

    #[test]
    fn file_without_a_known_shader_is_an_error() {
        let text = r#"<materialx><constant name="c" type="float" /></materialx>"#;
        assert!(parse(text, Path::new("/lib")).is_err());
    }
}
