//! Compare the byte layout of production Rust uniforms with the actual WGSL.
//! Unsupported Rust syntax fails closed; this is deliberately not a Rust compiler.
use std::{collections::BTreeMap, path::Path};
use syn::{
    Expr, Type,
    visit::{self, Visit},
};

#[derive(Clone, Debug, PartialEq)]
struct Leaf {
    name: String,
    offset: u32,
    kind: &'static str,
}
#[derive(Clone, Debug)]
struct Layout {
    size: u32,
    align: u32,
    leaves: Vec<Leaf>,
}
#[derive(Default)]
struct Source {
    defs: BTreeMap<String, syn::ItemStruct>,
    constants: BTreeMap<String, Expr>,
    duplicates: Vec<String>,
}

fn test_only(attrs: &[syn::Attribute]) -> bool {
    fn requires_test(meta: &syn::Meta) -> bool {
        match meta {
            syn::Meta::Path(p) => p.is_ident("test"),
            syn::Meta::List(list) if list.path.is_ident("all") || list.path.is_ident("any") => {
                use syn::parse::Parser;
                let parser =
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated;
                let Ok(items) = parser.parse2(list.tokens.clone()) else {
                    return false;
                };
                if list.path.is_ident("all") {
                    items.iter().any(requires_test)
                } else {
                    !items.is_empty() && items.iter().all(requires_test)
                }
            }
            _ => false,
        }
    }
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<syn::Meta>().is_ok_and(|m| requires_test(&m))
    })
}
impl<'a> Visit<'a> for Source {
    fn visit_item_mod(&mut self, item: &'a syn::ItemMod) {
        if !test_only(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }
    fn visit_item_fn(&mut self, item: &'a syn::ItemFn) {
        if !test_only(&item.attrs) {
            visit::visit_item_fn(self, item);
        }
    }
    fn visit_item_struct(&mut self, item: &'a syn::ItemStruct) {
        if test_only(&item.attrs) {
            return;
        }
        let name = item.ident.to_string();
        if self.defs.insert(name.clone(), item.clone()).is_some() {
            self.duplicates.push(name);
        }
    }
    fn visit_item_const(&mut self, item: &'a syn::ItemConst) {
        if !test_only(&item.attrs) {
            self.constants
                .insert(item.ident.to_string(), *item.expr.clone());
        }
    }
}
fn source(text: &str) -> Result<Source, String> {
    let file = syn::parse_file(text).map_err(|e| e.to_string())?;
    let mut out = Source::default();
    out.visit_file(&file);
    Ok(out)
}
fn integer(e: &Expr, src: &Source, depth: usize) -> Result<u32, String> {
    if depth > 32 {
        return Err("recursive constant".into());
    }
    match e {
        Expr::Lit(x) => match &x.lit {
            syn::Lit::Int(i) => i.base10_parse().map_err(|e| e.to_string()),
            _ => Err("non-integer array length".into()),
        },
        Expr::Path(p) if p.path.segments.len() == 1 => {
            let name = p.path.segments[0].ident.to_string();
            integer(
                src.constants
                    .get(&name)
                    .ok_or_else(|| format!("unknown constant {name}"))?,
                src,
                depth + 1,
            )
        }
        Expr::Paren(p) => integer(&p.expr, src, depth + 1),
        Expr::Binary(b) => {
            let a = integer(&b.left, src, depth + 1)?;
            let c = integer(&b.right, src, depth + 1)?;
            let n = match b.op {
                syn::BinOp::Add(_) => a.checked_add(c),
                syn::BinOp::Sub(_) => a.checked_sub(c),
                syn::BinOp::Mul(_) => a.checked_mul(c),
                syn::BinOp::Div(_) => a.checked_div(c),
                _ => return Err("unsupported constant operator".into()),
            };
            n.ok_or_else(|| "invalid/overflowing constant".into())
        }
        _ => Err("unsupported array length".into()),
    }
}
fn round_up(n: u32, align: u32) -> u32 {
    n.div_ceil(align) * align
}
fn prefix(layout: &Layout, name: &str, offset: u32, out: &mut Vec<Leaf>) {
    out.extend(layout.leaves.iter().map(|leaf| Leaf {
        name: if leaf.name.is_empty() {
            name.into()
        } else if leaf.name.starts_with('[') {
            format!("{name}{}", leaf.name)
        } else {
            format!("{name}.{}", leaf.name)
        },
        offset: offset + leaf.offset,
        kind: leaf.kind,
    }));
}
fn rust_type(ty: &Type, src: &Source, depth: usize) -> Result<Layout, String> {
    if depth > 32 {
        return Err("recursive Rust type".into());
    }
    match ty {
        Type::Array(array) => {
            let elem = rust_type(&array.elem, src, depth + 1)?;
            let count = integer(&array.len, src, 0)?;
            if count == 0 || count > 65536 {
                return Err("unsupported uniform array length".into());
            }
            let size = elem.size.checked_mul(count).ok_or("array size overflow")?;
            let mut leaves = Vec::new();
            for i in 0..count {
                prefix(&elem, &format!("[{i}]"), i * elem.size, &mut leaves);
            }
            Ok(Layout {
                size,
                align: elem.align,
                leaves,
            })
        }
        Type::Path(path) if path.qself.is_none() && path.path.segments.len() == 1 => {
            let name = path.path.segments[0].ident.to_string();
            let kind = match name.as_str() {
                "f32" => Some("f32"),
                "u32" => Some("u32"),
                "i32" => Some("i32"),
                _ => None,
            };
            if let Some(kind) = kind {
                return Ok(Layout {
                    size: 4,
                    align: 4,
                    leaves: vec![Leaf {
                        name: String::new(),
                        offset: 0,
                        kind,
                    }],
                });
            }
            rust_struct(&name, src, depth + 1)
        }
        _ => Err("unsupported Rust uniform type".into()),
    }
}
fn rust_struct(name: &str, src: &Source, depth: usize) -> Result<Layout, String> {
    if src.duplicates.iter().any(|n| n == name) {
        return Err(format!("ambiguous Rust struct {name}"));
    }
    let def = src
        .defs
        .get(name)
        .ok_or_else(|| format!("Rust struct {name} not found"))?;
    if !def.generics.params.is_empty() {
        return Err("generic uniform struct".into());
    }
    let mut c = false;
    let mut explicit_align: u32 = 1;
    for attr in &def.attrs {
        if attr.path().is_ident("repr") {
            attr.parse_nested_meta(|m| {
                if m.path.is_ident("C") {
                    c = true;
                    Ok(())
                } else if m.path.is_ident("align") {
                    let content;
                    syn::parenthesized!(content in m.input);
                    explicit_align = content.parse::<syn::LitInt>()?.base10_parse()?;
                    Ok(())
                } else {
                    Err(m.error("unsupported uniform representation"))
                }
            })
            .map_err(|e| e.to_string())?;
        } else if !attr.path().is_ident("derive")
            && !attr.path().is_ident("doc")
            && !attr.path().is_ident("allow")
        {
            return Err(format!("unsupported struct attribute on {name}"));
        }
    }
    if !c || !explicit_align.is_power_of_two() {
        return Err(format!("{name} requires repr(C) and valid alignment"));
    }
    let syn::Fields::Named(fields) = &def.fields else {
        return Err("unnamed uniform fields".into());
    };
    let mut out = Layout {
        size: 0,
        align: explicit_align,
        leaves: Vec::new(),
    };
    for field in &fields.named {
        if field.attrs.iter().any(|a| !a.path().is_ident("doc")) {
            return Err(format!("unsupported field attribute in {name}"));
        }
        let field_name = field.ident.as_ref().ok_or("unnamed field")?.to_string();
        let layout = rust_type(&field.ty, src, depth + 1)?;
        out.size = round_up(out.size, layout.align);
        prefix(&layout, &field_name, out.size, &mut out.leaves);
        out.size += layout.size;
        out.align = out.align.max(layout.align);
    }
    out.size = round_up(out.size, out.align);
    Ok(out)
}
fn scalar(s: naga::Scalar) -> Result<&'static str, String> {
    match (s.kind, s.width) {
        (naga::ScalarKind::Float, 4) => Ok("f32"),
        (naga::ScalarKind::Sint, 4) => Ok("i32"),
        (naga::ScalarKind::Uint, 4) => Ok("u32"),
        _ => Err("unsupported WGSL scalar".into()),
    }
}
fn reflect(
    module: &naga::Module,
    ty: naga::Handle<naga::Type>,
    name: &str,
    offset: u32,
    out: &mut Vec<Leaf>,
) -> Result<(), String> {
    match &module.types[ty].inner {
        naga::TypeInner::Scalar(s) => out.push(Leaf {
            name: name.into(),
            offset,
            kind: scalar(*s)?,
        }),
        naga::TypeInner::Vector { size, scalar: s } => {
            for i in 0..*size as u32 {
                out.push(Leaf {
                    name: format!("{name}[{i}]"),
                    offset: offset + i * u32::from(s.width),
                    kind: scalar(*s)?,
                });
            }
        }
        naga::TypeInner::Matrix {
            columns,
            rows,
            scalar: s,
        } => {
            let stride = if *rows == naga::VectorSize::Bi { 2 } else { 4 } * u32::from(s.width);
            for c in 0..*columns as u32 {
                for r in 0..*rows as u32 {
                    out.push(Leaf {
                        name: format!("{name}[{c}][{r}]"),
                        offset: offset + c * stride + r * u32::from(s.width),
                        kind: scalar(*s)?,
                    });
                }
            }
        }
        naga::TypeInner::Array { base, size, stride } => {
            let naga::ArraySize::Constant(count) = size else {
                return Err("runtime/override WGSL uniform array".into());
            };
            for i in 0..count.get() {
                reflect(
                    module,
                    *base,
                    &format!("{name}[{i}]"),
                    offset + i * stride,
                    out,
                )?;
            }
        }
        naga::TypeInner::Struct { members, .. } => {
            for m in members {
                let n = m.name.as_deref().ok_or("unnamed WGSL field")?;
                let path = if name.is_empty() {
                    n.into()
                } else {
                    format!("{name}.{n}")
                };
                reflect(module, m.ty, &path, offset + m.offset, out)?;
            }
        }
        _ => return Err(format!("unsupported WGSL uniform field {name}")),
    }
    Ok(())
}
fn is_pad(name: &str) -> bool {
    name.starts_with("_pad") || name.starts_with("pad")
}
fn canonical(name: &str) -> String {
    let name = name.strip_prefix("p_").unwrap_or(name);
    for (suffix, index) in [("_x", 0), ("_y", 1), ("_z", 2), ("_w", 3)] {
        if let Some(root) = name.strip_suffix(suffix) {
            return format!("{root}[{index}]");
        }
    }
    name.into()
}
fn alias(name: &str, aliases: &[(&str, &str)]) -> String {
    for (from, to) in aliases {
        if name == *from {
            return (*to).into();
        }
        if let Some(suffix) = name.strip_prefix(from)
            && (suffix.starts_with('[') || suffix.starts_with('.'))
        {
            return format!("{to}{suffix}");
        }
    }
    name.into()
}
fn compare_source(
    text: &str,
    rust_name: &str,
    wgsl: &str,
    shader_name: &str,
    aliases: &[(&str, &str)],
) -> Result<(), String> {
    let rust = rust_struct(rust_name, &source(text)?, 0)?;
    let module = naga::front::wgsl::parse_str(wgsl).map_err(|e| e.emit_to_string(wgsl))?;
    let (id, ty) = module
        .types
        .iter()
        .find(|(_, t)| t.name.as_deref() == Some(shader_name))
        .ok_or("WGSL struct not found")?;
    let naga::TypeInner::Struct { span, .. } = &ty.inner else {
        return Err("WGSL type is not a struct".into());
    };
    let mut shader = Vec::new();
    reflect(&module, id, "", 0, &mut shader)?;
    if rust.size != *span {
        return Err(format!("span mismatch: Rust {}, WGSL {span}", rust.size));
    }
    if rust.leaves.len() != shader.len() {
        return Err(format!(
            "leaf count mismatch: Rust {}, WGSL {}",
            rust.leaves.len(),
            shader.len()
        ));
    }
    for (r, s) in rust.leaves.iter().zip(&shader) {
        let name = alias(&r.name, aliases);
        let padding = is_pad(&name) && is_pad(&s.name);
        if r.offset != s.offset
            || (!padding && (r.kind != s.kind || canonical(&name) != canonical(&s.name)))
        {
            return Err(format!(
                "ABI mismatch: Rust {r:?}, WGSL {s:?} (host alias {name})"
            ));
        }
    }
    Ok(())
}
pub fn assert_wgsl_layout(
    path: &Path,
    rust_name: &str,
    wgsl: &str,
    shader_name: &str,
    aliases: &[(&str, &str)],
) -> Result<(), String> {
    compare_source(
        &std::fs::read_to_string(path).map_err(|e| e.to_string())?,
        rust_name,
        wgsl,
        shader_name,
        aliases,
    )
}

/// The census includes every production repr struct, even if its name does not
/// contain Uniform. A changed or newly added mirror must acquire a proof.
pub fn host_structs(path: &Path) -> Result<Vec<(String, bool)>, String> {
    let src = source(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)?;
    Ok(src
        .defs
        .into_iter()
        .filter(|(_, d)| d.attrs.iter().any(|a| a.path().is_ident("repr")))
        .map(|(n, d)| {
            (
                n,
                d.fields
                    .iter()
                    .any(|f| f.ident.as_ref().is_some_and(|i| i == "dispatch_count")),
            )
        })
        .collect())
}

/// Extract the actual declaration from custom shaders. Their function bodies
/// have runtime template substitutions and shared includes, which do not affect
/// these uniform declarations. Naga still reflects every declared field.
pub fn shader_declaration(wgsl: &str, name: &str) -> Result<String, String> {
    let mut clean = String::new();
    let mut chars = wgsl.chars().peekable();
    let mut block_depth = 0;
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            block_depth += 1;
            continue;
        }
        if block_depth > 0 {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                block_depth -= 1;
                clean.push(' ');
            }
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    clean.push('\n');
                    break;
                }
            }
        } else {
            clean.push(c);
        }
    }
    let tokens: Vec<&str> = clean
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| !t.is_empty())
        .collect();
    if tokens
        .windows(2)
        .filter(|t| t[0] == "struct" && t[1] == name)
        .count()
        != 1
    {
        return Err(format!("expected one WGSL declaration for {name}"));
    }
    for (index, _) in clean.match_indices("struct") {
        let rest = clean[index + 6..].trim_start();
        if let Some(fields) = rest.strip_prefix(name)
            && fields.trim_start().starts_with('{')
        {
            let start = clean[index..]
                .find('{')
                .ok_or("missing struct opening brace")?
                + index;
            let end = clean[start..]
                .find('}')
                .ok_or("missing struct closing brace")?
                + start;
            return Ok(clean[index..=end].into());
        }
    }
    Err(format!("WGSL struct {name} not found"))
}

pub fn rust_string_constant(path: &Path, name: &str) -> Result<String, String> {
    let src = source(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)?;
    match src.constants.get(name) {
        Some(Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        })) => Ok(s.value()),
        _ => Err(format!("literal shader constant {name} not found")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const HOST: &str = "#[repr(C)] struct U { a: f32, b: u32, c: [f32; 2] }";
    const SHADER: &str = "struct U { a: f32, b: u32, c: vec2<f32> }";
    #[test]
    fn rejects_type_offset_span_and_semantic_drift() {
        assert!(compare_source(HOST, "U", SHADER, "U", &[]).is_ok());
        for bad in [
            SHADER.replace("b: u32", "b: f32"),
            SHADER.replace("c: vec2<f32>", "c: vec4<f32>"),
            SHADER.replace("a: f32, b: u32", "b: u32, a: f32"),
        ] {
            assert!(
                compare_source(HOST, "U", &bad, "U", &[]).is_err(),
                "accepted {bad}"
            );
        }
        assert!(
            compare_source(
                HOST,
                "U",
                &SHADER.replace("b: u32", "@align(16) b: u32"),
                "U",
                &[]
            )
            .is_err()
        );
        assert!(
            compare_source(
                "#[repr(C)] struct U { a: f32, b: f32 }",
                "U",
                "struct U { b: f32, a: f32 }",
                "U",
                &[]
            )
            .is_err()
        );
    }
    #[test]
    fn arrays_matrices_and_stride() {
        let host = "const N: usize = 2; #[repr(C)] struct U { table: [[f32; 4]; N], matrix: [[f32; 4]; 4] }";
        let shader = "struct U { table: array<vec4<f32>, 2>, matrix: mat4x4<f32> }";
        assert!(compare_source(host, "U", shader, "U", &[]).is_ok());
        let bad = "#[repr(C)] struct U { table: [[f32; 3]; 2] }";
        assert!(
            compare_source(
                bad,
                "U",
                "struct U { table: array<vec3<f32>, 2> }",
                "U",
                &[]
            )
            .is_err()
        );
    }
    #[test]
    fn rejects_unsupported_host_representations_and_conditional_fields() {
        for host in [
            "#[repr(packed)] struct U { a: f32 }",
            "#[repr(C)] struct U { #[cfg(unix)] a: f32 }",
            "#[repr(C)] struct U { a: f64 }",
        ] {
            assert!(compare_source(host, "U", "struct U { a: f32 }", "U", &[]).is_err());
        }
    }
}
