//! Reads the two shipped v1 model files (recipe version 1, `assets/{kick,snare}_model.mkick`) as contract v2,
//! so the generic runtime runs them until P3 swaps in the files the release tool converts
//! (docs/DETECTOR_RUNTIME_DESIGN.md D3, section 5.4). Same numbers, v2 names; nothing is retuned.
//! Deleted at P3 with the v1 files.

use super::container::{Array, Container, ContainerError, Data};
use super::spec::{MODEL_HOP, MODEL_RATE};

pub const RECIPE_VERSION: i32 = 1;

#[derive(Debug, Clone, PartialEq)]
pub enum V1Error {
    Model(ContainerError),
    /// The file's `recipe_version` is not [`RECIPE_VERSION`].
    Recipe(i32),
    /// A v1 entry the v2 contract has no place for, with another value than the one v1 always wrote.
    Unexpected(&'static str),
}

impl From<ContainerError> for V1Error {
    fn from(e: ContainerError) -> Self {
        Self::Model(e)
    }
}

impl std::fmt::Display for V1Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Model(e) => write!(f, "{e}"),
            Self::Recipe(v) => write!(f, "v1 model recipe_version {v}, this build reads {RECIPE_VERSION}"),
            Self::Unexpected(name) => write!(f, "v1 model: unexpected {name}"),
        }
    }
}

impl std::error::Error for V1Error {}

fn recipe(c: &Container) -> Result<(), V1Error> {
    let v = c.i32s("recipe_version", &[])?[0];
    if v != RECIPE_VERSION {
        return Err(V1Error::Recipe(v));
    }
    Ok(())
}

fn take(c: &mut Container, name: &str) -> Result<Array, V1Error> {
    c.remove(name).ok_or_else(|| ContainerError::Missing(name.to_owned()).into())
}

fn rename(c: &mut Container, from: &str, to: &str) -> Result<(), V1Error> {
    let a = take(c, from)?;
    c.insert(to, a);
    Ok(())
}

fn f64_scalar(v: f64) -> Array {
    Array::f64s(&[], vec![v])
}

fn i64_scalar(v: i64) -> Array {
    Array::i64s(&[], vec![v])
}

/// v1 net weights `net.<param>` become net 0's; the input entries keep their names.
fn net_weights(c: &mut Container) -> Result<(), V1Error> {
    for p in super::spec::NET_PARAMS {
        rename(c, &format!("net.{p}"), &format!("net.0.{p}"))?;
    }
    c.insert("net.count", i64_scalar(1));
    Ok(())
}

fn header(c: &mut Container, family: &str, finder: &str, models: &str, combine: &str) {
    c.insert("contract", Array::i32_scalar(super::spec::CONTRACT));
    c.insert("family", Array::text(family));
    c.insert("config", Array::text("{}"));
    c.insert("run_id", Array::text("v1"));
    c.insert("rate", i64_scalar(MODEL_RATE as i64));
    c.insert("hop", i64_scalar(MODEL_HOP as i64));
    c.insert("finder", Array::text(finder));
    c.insert("models", Array::text(models));
    c.insert("combine", Array::text(combine));
}

fn expect_f64(c: &Container, name: &'static str, want: &[f64]) -> Result<(), V1Error> {
    if c.f64s(name, &[want.len()])? != want {
        return Err(V1Error::Unexpected(name));
    }
    Ok(())
}

fn expect_i64(c: &Container, name: &'static str, want: i64) -> Result<(), V1Error> {
    if c.i64s(name, &[])?[0] != want {
        return Err(V1Error::Unexpected(name));
    }
    Ok(())
}

/// The shipped kick (recipe 1): kick finder, trees on f69 = f15 tail3 tmpl3 prof32 low16, a
/// 150 ms net, mean-logit blend, the 8 s stage on hstack(prof32, low16) with level f15[0], no gate.
pub fn kick(v1: &Container) -> Result<Container, V1Error> {
    recipe(v1)?;
    let mut c = v1.clone();
    c.remove("recipe_version");
    header(&mut c, "kick", "kick", "net,trees", "mean_logit");
    net_weights(&mut c)?;
    let head = c.shape("net.0.head.0.weight")?;
    let tbins = head.get(1).copied().unwrap_or(0) / (32 * 16);
    c.insert("net.channels", i64_scalar(2));
    c.insert("net.tbins", i64_scalar(tbins as i64));
    c.insert("net.extra", Array::text(""));
    for k in ["all", "low", "high"] {
        rename(&mut c, &format!("extra.templates_{k}"), &format!("fit.tmpl3.{k}"))?;
    }
    let span = c.get("extra.patch_span")?.clone();
    c.insert("fit.tmpl3.span", span);
    c.insert("trees.measures", Array::text("f15,tail3,tmpl3,prof32,low16"));
    c.insert("trees.widths", Array::i64s(&[5], vec![15, 3, 3, 32, 16]));
    rename(&mut c, "stage.cutoff", "cutoff")?;
    rename(&mut c, "stage.refractory_s", "refractory_s")?;
    rename(&mut c, "stage.blend_clip", "combine.clip")?;
    expect_f64(&c, "stage.blend_weights", &[0.5, 0.5])?;
    expect_i64(&c, "stage.sample_rate", MODEL_RATE as i64)?;
    expect_i64(&c, "stage.hop", MODEL_HOP as i64)?;
    for gone in ["stage.blend_weights", "stage.sample_rate", "stage.hop"] {
        c.remove(gone);
    }
    let window = take(&mut c, "stage.window_s")?;
    let Data::F64(w) = window.data else { return Err(ContainerError::WrongType { name: "stage.window_s".into(), want: "f64" }.into()) };
    c.insert("stage", Array::i32_scalar(1));
    c.insert("stage.windows_s", Array::f64s(&[w.len()], w));
    c.insert("stage.shape", Array::text("prof32,low16"));
    c.insert("stage.level", Array::text("f15:0"));
    c.insert("stage.gate", f64_scalar(0.0));
    c.insert("stage.q_clip", f64_scalar(1e-6));
    Ok(c)
}

/// The shipped snare: snare finder, a 190 ms net on shape, new and flat, no trees, no stage.
pub fn snare(v1: &Container) -> Result<Container, V1Error> {
    recipe(v1)?;
    let mut c = v1.clone();
    c.remove("recipe_version");
    expect_i64(&c, "sample_rate", MODEL_RATE as i64)?;
    c.remove("sample_rate");
    let db = c.f64s("cand.rise_db", &[])?[0];
    header(&mut c, "snare", &format!("snare:{db}"), "net", "net");
    net_weights(&mut c)?;
    c.insert("net.extra", Array::text("flat"));
    c.insert("combine.clip", f64_scalar(1e-6));
    c.insert("stage", Array::i32_scalar(0));
    Ok(c)
}
