//! Contract v2 (docs/DETECTOR_RUNTIME_DESIGN.md section 4): what a model file says it is built from, checked
//! before any part is built. Every unknown, missing or inconsistent entry is a named refusal (D8); nothing is
//! skipped. The parts this runtime implements are declared in `tests/fixtures/detectors/runtime_parts.json`.

use super::container::{Container, ContainerError};
use crate::detectors::EventKind;

pub const CONTRACT: i32 = 2;
pub const MODEL_RATE: u32 = 48_000;
pub const MODEL_HOP: u64 = 256;
pub const MAX_WINDOWS: usize = 4;
/// The fewest nets a model with a net averages (D17).
pub const MIN_NETS: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinderKind {
    Kick,
    Snare,
}

impl FinderKind {
    pub const ALL: [FinderKind; 2] = [FinderKind::Kick, FinderKind::Snare];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Kick => "kick",
            Self::Snare => "snare",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Measure {
    F15,
    Tail3,
    Tmpl3,
    Prof32,
    Fall16,
    Low16,
    Cdom4,
    Note13,
    /// The kick detector's verdict: read from the kick's decisions, not the audio.
    Kick2,
}

impl Measure {
    pub const ALL: [Measure; 9] = [
        Self::F15,
        Self::Tail3,
        Self::Tmpl3,
        Self::Prof32,
        Self::Fall16,
        Self::Low16,
        Self::Cdom4,
        Self::Note13,
        Self::Kick2,
    ];

    /// The lab's names (`lab/components.py` `MEASURES`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::F15 => "f15",
            Self::Tail3 => "tail3",
            Self::Tmpl3 => "tmpl3",
            Self::Prof32 => "prof32",
            Self::Fall16 => "fall16",
            Self::Low16 => "low16",
            Self::Cdom4 => "cdom4",
            Self::Note13 => "note13",
            Self::Kick2 => "kick2",
        }
    }

    /// Whether the measure reads the `extra.*` constants (f15 is the kick finder's, kick2 the kick's).
    pub const fn uses_extra(self) -> bool {
        !matches!(self, Self::F15 | Self::Kick2)
    }

    pub const fn width(self) -> usize {
        match self {
            Self::F15 => 15,
            Self::Tail3 | Self::Tmpl3 => 3,
            Self::Prof32 => 32,
            Self::Fall16 | Self::Low16 => 16,
            Self::Cdom4 => 4,
            Self::Note13 => 13,
            Self::Kick2 => 2,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == s)
    }
}

/// Extra net input channels on the net's frame grid (`lab/components.py` `FRAME_MEASURES`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Flat,
    Perc,
}

impl Channel {
    pub const ALL: [Channel; 2] = [Self::Flat, Self::Perc];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Perc => "perc",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combine {
    Net,
    Trees,
    MeanLogit,
}

impl Combine {
    pub const ALL: [Combine; 3] = [Self::Net, Self::Trees, Self::MeanLogit];

    /// The lab's names (`lab/config.py` `COMBINES`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Net => "net",
            Self::Trees => "trees",
            Self::MeanLogit => "mean_logit",
        }
    }

    /// The `models` text this combine reads.
    const fn models(self) -> &'static str {
        match self {
            Self::Net => "net",
            Self::Trees => "trees",
            Self::MeanLogit => "net,trees",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.name() == s)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NetSpec {
    /// N >= 1 nets averaged (D17).
    pub count: usize,
    /// Extra channels in order.
    pub channels: Vec<Channel>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StageSpec {
    pub windows_s: Vec<f64>,
    pub shape: Vec<Measure>,
    pub level: Option<(Measure, usize)>,
    pub gate: f64,
    /// (cut_detector, cut_stage); `Some` iff gate > 0.
    pub cuts: Option<(f64, f64)>,
    pub power: f64,
    pub lp_clip: f64,
    pub q_clip: f64,
}

impl StageSpec {
    /// Stage inputs: lp, then per window sim, lp_rel, lvl_rel (with a level) and evidence.
    pub fn inputs(&self) -> usize {
        1 + self.windows_s.len() * (3 + self.level.is_some() as usize)
    }

    /// Index of window 0's evidence in the stage inputs (the gate reads it).
    pub fn evidence0(&self) -> usize {
        3 + self.level.is_some() as usize
    }

    pub fn shape_width(&self) -> usize {
        self.shape.iter().map(|m| m.width()).sum()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub family: EventKind,
    pub train_id: String,
    pub finder: FinderKind,
    /// Column order.
    pub trees: Option<Vec<Measure>>,
    pub net: Option<NetSpec>,
    pub combine: Combine,
    pub combine_clip: f64,
    pub stage: Option<StageSpec>,
    pub cutoff: f64,
    pub refractory_s: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpecError {
    Container(ContainerError),
    Contract(Option<i32>),
    Unknown { what: &'static str, name: String },
    Mismatch { what: &'static str, detail: String },
}

impl std::fmt::Display for SpecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Container(e) => write!(f, "{e}"),
            Self::Contract(None) => write!(
                f,
                "model file has no contract entry; this build runs contract {CONTRACT}"
            ),
            Self::Contract(Some(v)) => write!(
                f,
                "model file contract {v}; this build runs contract {CONTRACT}"
            ),
            Self::Unknown { what, name } => write!(f, "model file: unknown {what} {name:?}"),
            Self::Mismatch { what, detail } => write!(f, "model file: {what}: {detail}"),
        }
    }
}

impl std::error::Error for SpecError {}

impl From<ContainerError> for SpecError {
    fn from(e: ContainerError) -> Self {
        Self::Container(e)
    }
}

fn mismatch(what: &'static str, detail: impl Into<String>) -> SpecError {
    SpecError::Mismatch {
        what,
        detail: detail.into(),
    }
}

fn unknown(what: &'static str, name: &str) -> SpecError {
    SpecError::Unknown {
        what,
        name: name.to_owned(),
    }
}

/// The weights of one net, PyTorch names (`kick_goal_nn.Net`).
pub(crate) const NET_PARAMS: [&str; 10] = [
    "conv.0.weight",
    "conv.0.bias",
    "conv.3.weight",
    "conv.3.bias",
    "conv.6.weight",
    "conv.6.bias",
    "head.0.weight",
    "head.0.bias",
    "head.2.weight",
    "head.2.bias",
];
const FOREST: [&str; 10] = [
    "value",
    "threshold",
    "feature",
    "left",
    "right",
    "missing_left",
    "is_leaf",
    "offsets",
    "baseline",
    "n_features",
];
const TOP: [&str; 14] = [
    "contract",
    "family",
    "train_id",
    "config",
    "run_id",
    "rate",
    "hop",
    "finder",
    "models",
    "combine",
    "combine.clip",
    "cutoff",
    "refractory_s",
    "stage",
];
const BASE: [&str; 19] = [
    "active_floor",
    "band_sos",
    "band_splits",
    "body_ratio",
    "deadline",
    "env_alpha",
    "env_gain",
    "eps",
    "fft_bins",
    "fft_window",
    "hop",
    "lag_norm_s",
    "log2_freq",
    "log_clip",
    "low_ratio",
    "sample_rate",
    "span",
    "upper_count",
    "upper_ratio",
];
const CAND: [&str; 7] = [
    "deadline",
    "hop",
    "lookback",
    "refractory",
    "rise_db",
    "sos",
    "window",
];
const EXTRA: [&str; 28] = [
    "band_high",
    "band_low",
    "cdom_bin_lo",
    "cdom_bin_n",
    "cdom_floor",
    "cdom_raw_eps",
    "cdom_window",
    "note_chroma_bins",
    "note_chroma_class",
    "note_low_rate",
    "note_lp_sos",
    "note_mid_bins",
    "note_window",
    "hop",
    "low_floor",
    "low_ma_len",
    "low_sos",
    "ms",
    "patch_span",
    "sample_rate",
    "spec_band_of_bin",
    "spec_floor",
    "spec_window",
    "tail_floor",
    "tail_ma_len",
    "tail_past_ms",
    "tail_sos",
    "unit_eps",
];
const FIT_TMPL3: [&str; 4] = ["all", "low", "high", "span"];
const NET: [&str; 14] = [
    "sos",
    "k",
    "frame_s",
    "frame_hop",
    "slice",
    "pre_s",
    "span",
    "ahead_s",
    "hop",
    "sample_rate",
    "channels",
    "tbins",
    "extra",
    "count",
];
const FLAT: [&str; 5] = ["n_fft", "window", "band_lo", "band_n", "clip_db"];
const PERC: [&str; 2] = ["sustain", "neighbours"];
const KICK2: [&str; 3] = ["window_s", "none_dt_ms", "kick_train_id"];
const STAGE: [&str; 9] = [
    "windows_s",
    "shape",
    "level",
    "gate",
    "cut_detector",
    "cut_stage",
    "power",
    "lp_clip",
    "q_clip",
];
/// Each part's own grid entries; all must be the model grid (D13).
const GRIDS: [(&str, u64); 7] = [
    ("base.hop", MODEL_HOP),
    ("base.sample_rate", MODEL_RATE as u64),
    ("cand.hop", MODEL_HOP),
    ("extra.hop", MODEL_HOP),
    ("extra.sample_rate", MODEL_RATE as u64),
    ("net.hop", MODEL_HOP),
    ("net.sample_rate", MODEL_RATE as u64),
];

fn scalar_f64(c: &Container, name: &str) -> Result<f64, SpecError> {
    Ok(c.f64s(name, &[])?[0])
}

fn scalar_i64(c: &Container, name: &str) -> Result<i64, SpecError> {
    Ok(c.i64s(name, &[])?[0])
}

/// A comma-separated list; empty text is the empty list.
fn list(c: &Container, name: &str) -> Result<Vec<String>, SpecError> {
    let t = c.text(name)?;
    Ok(if t.is_empty() {
        Vec::new()
    } else {
        t.split(',').map(str::to_owned).collect()
    })
}

fn measures(c: &Container, name: &str) -> Result<Vec<Measure>, SpecError> {
    let names = list(c, name)?;
    let mut out = Vec::with_capacity(names.len());
    for n in &names {
        let m = Measure::parse(n).ok_or_else(|| unknown("measure", n))?;
        if out.contains(&m) {
            return Err(mismatch("measure list", format!("{name} names {n} twice")));
        }
        out.push(m);
    }
    if out.is_empty() {
        return Err(mismatch("measure list", format!("{name} is empty")));
    }
    Ok(out)
}

impl Spec {
    pub fn parse(c: &Container) -> Result<Self, SpecError> {
        if !c.contains("contract") {
            return Err(SpecError::Contract(None));
        }
        let contract = c.i32s("contract", &[])?[0];
        if contract != CONTRACT {
            return Err(SpecError::Contract(Some(contract)));
        }
        let family = c.text("family")?;
        let family = EventKind::parse(family).ok_or_else(|| unknown("family", family))?;
        let train_id = c.text("train_id")?.to_owned();
        c.text("config")?;
        c.text("run_id")?;
        for (name, want) in [("rate", MODEL_RATE as i64), ("hop", MODEL_HOP as i64)] {
            let got = scalar_i64(c, name)?;
            if got != want {
                return Err(mismatch(
                    "model grid",
                    format!("{name} {got}, this build runs {want}"),
                ));
            }
        }
        for (name, want) in GRIDS {
            if c.contains(name) && scalar_i64(c, name)? != want as i64 {
                return Err(mismatch(
                    "model grid",
                    format!("{name} {} disagrees with {want}", scalar_i64(c, name)?),
                ));
            }
        }

        let finder_text = c.text("finder")?;
        let finder = match finder_text.split_once(':') {
            None if finder_text == "kick" => FinderKind::Kick,
            Some(("snare", db)) => {
                let db: f64 = db.parse().map_err(|_| unknown("finder", finder_text))?;
                let file = scalar_f64(c, "cand.rise_db")?;
                if db != file {
                    return Err(mismatch(
                        "finder",
                        format!("{finder_text} but cand.rise_db is {file}"),
                    ));
                }
                FinderKind::Snare
            }
            _ => return Err(unknown("finder", finder_text)),
        };

        let models = c.text("models")?;
        let (has_net, has_trees) = match models {
            "net" => (true, false),
            "trees" => (false, true),
            "net,trees" => (true, true),
            other => return Err(unknown("models", other)),
        };
        let combine_text = c.text("combine")?;
        let combine =
            Combine::parse(combine_text).ok_or_else(|| unknown("combine", combine_text))?;
        if combine.models() != models {
            return Err(mismatch(
                "combine",
                format!("{combine_text} with models {models}"),
            ));
        }

        let trees = if has_trees {
            let ms = measures(c, "trees.measures")?;
            let widths = c.i64s("trees.widths", &[ms.len()])?;
            for (m, &w) in ms.iter().zip(widths) {
                if w != m.width() as i64 {
                    return Err(mismatch(
                        "measure width",
                        format!("{} is {} wide, the file says {w}", m.name(), m.width()),
                    ));
                }
            }
            let sum: usize = ms.iter().map(|m| m.width()).sum();
            let n = scalar_i64(c, "trees.n_features")?;
            if n != sum as i64 {
                return Err(mismatch(
                    "forest width",
                    format!("trees.n_features {n}, its measures are {sum} wide"),
                ));
            }
            Some(ms)
        } else {
            None
        };

        let net = if has_net { Some(net_spec(c)?) } else { None };

        let stage = match c.i32s("stage", &[])?[0] {
            0 => None,
            1 => Some(stage_spec(c)?),
            v => return Err(mismatch("stage", format!("stage {v}, want 0 or 1"))),
        };

        let spec = Self {
            family,
            train_id,
            finder,
            trees,
            net,
            combine,
            combine_clip: scalar_f64(c, "combine.clip")?,
            stage,
            cutoff: scalar_f64(c, "cutoff")?,
            refractory_s: scalar_f64(c, "refractory_s")?,
        };
        let used = spec.measures();
        if used.contains(&Measure::F15) && finder != FinderKind::Kick {
            return Err(mismatch(
                "f15",
                format!("f15 comes with the kick finder, this model's finder is {finder_text}"),
            ));
        }
        if used.contains(&Measure::Tmpl3) {
            for key in FIT_TMPL3 {
                if !c.contains(&format!("fit.tmpl3.{key}")) {
                    return Err(mismatch(
                        "fitted measure",
                        format!("tmpl3 without fit.tmpl3.{key}"),
                    ));
                }
            }
            if c.contains("extra.patch_span")
                && scalar_i64(c, "extra.patch_span")? != scalar_i64(c, "fit.tmpl3.span")?
            {
                return Err(mismatch(
                    "fitted measure",
                    "extra.patch_span differs from fit.tmpl3.span",
                ));
            }
        }
        spec.check_names(c)?;
        Ok(spec)
    }

    /// Every measure the model reads (trees, stage shape, level), first use order.
    pub fn measures(&self) -> Vec<Measure> {
        let mut out: Vec<Measure> = Vec::new();
        let stage = self
            .stage
            .iter()
            .flat_map(|s| s.shape.iter().copied().chain(s.level.map(|l| l.0)));
        for m in self.trees.iter().flatten().copied().chain(stage) {
            if !out.contains(&m) {
                out.push(m);
            }
        }
        out
    }

    /// Every entry must belong to a part this model uses; an entry for an unused part is as wrong as an
    /// unknown one, since the release wrote something the runtime would ignore.
    fn check_names(&self, c: &Container) -> Result<(), SpecError> {
        let used = self.measures();
        let extra = used.iter().any(|&m| m.uses_extra());
        let flat = self
            .net
            .as_ref()
            .is_some_and(|n| n.channels.contains(&Channel::Flat));
        let perc = self
            .net
            .as_ref()
            .is_some_and(|n| n.channels.contains(&Channel::Perc));
        for name in c.names() {
            let (prefix, rest) = name.split_once('.').unwrap_or((name, ""));
            let (part, known, active) = match prefix {
                _ if TOP.contains(&name) => continue,
                "base" => (
                    "the kick finder",
                    BASE.contains(&rest),
                    self.finder == FinderKind::Kick,
                ),
                "cand" => (
                    "the snare finder",
                    CAND.contains(&rest),
                    self.finder == FinderKind::Snare,
                ),
                "extra" => ("event measures", EXTRA.contains(&rest), extra),
                "fit" => (
                    "a fitted measure",
                    rest.strip_prefix("tmpl3.")
                        .is_some_and(|k| FIT_TMPL3.contains(&k)),
                    used.contains(&Measure::Tmpl3),
                ),
                "net" => (
                    "a net",
                    NET.contains(&rest) || net_weight(rest).is_some(),
                    self.net.is_some(),
                ),
                "flat" => ("the flat channel", FLAT.contains(&rest), flat),
                "perc" => ("the perc channel", PERC.contains(&rest), perc),
                "kick2" => (
                    "the kick verdict",
                    KICK2.contains(&rest),
                    used.contains(&Measure::Kick2),
                ),
                "trees" => (
                    "trees",
                    rest == "measures" || rest == "widths" || FOREST.contains(&rest),
                    self.trees.is_some(),
                ),
                "stage" => (
                    "a history stage",
                    STAGE.contains(&rest) || FOREST.contains(&rest),
                    self.stage.is_some(),
                ),
                _ => ("", false, false),
            };
            if !known {
                return Err(unknown("entry", name));
            }
            if !active {
                return Err(mismatch(
                    "entry",
                    format!("{name}, but the model has no {part}"),
                ));
            }
        }
        Ok(())
    }
}

/// `<k>.<param>` of a net weight: `(k, param)`.
fn net_weight(rest: &str) -> Option<(usize, &str)> {
    let (k, param) = rest.split_once('.')?;
    let k: usize = k.parse().ok()?;
    NET_PARAMS.contains(&param).then_some((k, param))
}

fn net_spec(c: &Container) -> Result<NetSpec, SpecError> {
    let count = scalar_i64(c, "net.count")?;
    if count < MIN_NETS as i64 {
        return Err(mismatch(
            "net.count",
            format!("{count}, want at least {MIN_NETS}"),
        ));
    }
    let count = count as usize;
    let mut channels = Vec::new();
    for n in list(c, "net.extra")? {
        channels.push(Channel::parse(&n).ok_or_else(|| unknown("channel", &n))?);
    }
    let declared = scalar_i64(c, "net.channels")?;
    if declared != channels.len() as i64 + 2 {
        return Err(mismatch(
            "net channels",
            format!(
                "net.channels {declared} with extra channels {:?}",
                c.text("net.extra")?
            ),
        ));
    }
    for name in c.names() {
        if let Some((k, _)) = name.strip_prefix("net.").and_then(net_weight)
            && k >= count
        {
            return Err(mismatch(
                "net weights",
                format!("{name} for a net past net.count {count}"),
            ));
        }
    }
    for param in NET_PARAMS {
        let first = c.shape(&format!("net.0.{param}"))?;
        for k in 1..count {
            let other = c.shape(&format!("net.{k}.{param}"))?;
            if other != first {
                return Err(mismatch(
                    "net weights",
                    format!("net.{k}.{param} shape {other:?}, net 0 has {first:?}"),
                ));
            }
        }
    }
    Ok(NetSpec { count, channels })
}

fn stage_spec(c: &Container) -> Result<StageSpec, SpecError> {
    let w = c.shape("stage.windows_s")?;
    let n = if w.len() == 1 { w[0] } else { 0 };
    if !(1..=MAX_WINDOWS).contains(&n) {
        return Err(mismatch(
            "stage.windows_s",
            format!("shape {w:?}, want 1 to {MAX_WINDOWS} windows"),
        ));
    }
    let windows_s = c.f64s("stage.windows_s", &[n])?.to_vec();
    if windows_s.iter().any(|w| !(*w > 0.0 && *w <= 60.0)) {
        return Err(mismatch(
            "stage.windows_s",
            format!("{windows_s:?}, want seconds in (0, 60]"),
        ));
    }
    let shape = measures(c, "stage.shape")?;
    let level_text = c.text("stage.level")?;
    let level = if level_text.is_empty() {
        None
    } else {
        let (m, col) = level_text
            .split_once(':')
            .ok_or_else(|| mismatch("stage.level", level_text))?;
        let m = Measure::parse(m).ok_or_else(|| unknown("measure", m))?;
        let col: usize = col
            .parse()
            .map_err(|_| mismatch("stage.level", level_text))?;
        if col >= m.width() {
            return Err(mismatch(
                "stage.level",
                format!("{level_text}: {} has {} columns", m.name(), m.width()),
            ));
        }
        Some((m, col))
    };
    let gate = scalar_f64(c, "stage.gate")?;
    if gate.is_nan() || gate < 0.0 {
        return Err(mismatch("stage.gate", format!("{gate}")));
    }
    let has_cuts = [
        c.contains("stage.cut_detector"),
        c.contains("stage.cut_stage"),
    ];
    let cuts = if gate > 0.0 {
        if has_cuts != [true, true] {
            return Err(mismatch(
                "stage cuts",
                format!("gate {gate} needs stage.cut_detector and stage.cut_stage"),
            ));
        }
        Some((
            scalar_f64(c, "stage.cut_detector")?,
            scalar_f64(c, "stage.cut_stage")?,
        ))
    } else {
        if has_cuts != [false, false] {
            return Err(mismatch("stage cuts", "gate 0 uses no cuts"));
        }
        None
    };
    let spec = StageSpec {
        windows_s,
        shape,
        level,
        gate,
        cuts,
        power: scalar_f64(c, "stage.power")?,
        lp_clip: scalar_f64(c, "stage.lp_clip")?,
        q_clip: scalar_f64(c, "stage.q_clip")?,
    };
    let nf = scalar_i64(c, "stage.n_features")?;
    if nf != spec.inputs() as i64 {
        return Err(mismatch(
            "forest width",
            format!(
                "stage.n_features {nf}, the stage has {} inputs",
                spec.inputs()
            ),
        ));
    }
    Ok(spec)
}

#[cfg(test)]
mod tests;
