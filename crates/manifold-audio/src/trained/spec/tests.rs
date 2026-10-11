use super::*;
use crate::trained::container::Array;
use crate::trained::golden;

#[test]
fn reads_both_shipped_models() {
    let kick = Spec::parse(&golden::model("kick")).unwrap();
    assert_eq!((kick.family, kick.finder, kick.combine), (EventKind::Kick, FinderKind::Kick, Combine::MeanLogit));
    let trees = [Measure::F15, Measure::Tail3, Measure::Prof32, Measure::Fall16, Measure::Low16, Measure::Note13, Measure::Cdom4];
    assert_eq!(kick.trees.as_deref(), Some(&trees[..]));
    assert_eq!(kick.net, Some(NetSpec { count: 1, channels: vec![Channel::Flat, Channel::Perc] }));
    assert_eq!(kick.measures(), trees);
    let st = kick.stage.as_ref().unwrap();
    assert_eq!((st.inputs(), st.evidence0(), st.gate, st.cuts), (5, 4, 1.0, Some((0.7100000000000001, 0.93))));
    let snare = Spec::parse(&golden::model("snare")).unwrap();
    assert_eq!((snare.family, snare.finder, snare.combine), (EventKind::Snare, FinderKind::Snare, Combine::MeanLogit));
    assert_eq!(snare.trees.as_deref(), Some(&[Measure::Prof32, Measure::Fall16, Measure::Low16][..]));
    assert_eq!(snare.net, Some(NetSpec { count: 3, channels: vec![] }));
    let st = snare.stage.as_ref().unwrap();
    assert_eq!((st.inputs(), st.evidence0(), st.level, st.gate), (4, 3, None, 1.0));
}

/// The shipped kick with tmpl3 in its trees (no shipped model uses it): templates from the tmpl3 unit fixture.
fn kick_tmpl3() -> Container {
    let mut c = golden::model("kick");
    let u = golden::unit("tmpl3.mkick");
    for n in ["fit.tmpl3.all", "fit.tmpl3.low", "fit.tmpl3.high", "fit.tmpl3.span"] {
        c.insert(n, u.get(n).unwrap().clone());
    }
    text(&mut c, "trees.measures", "f15,tail3,tmpl3,prof32,low16");
    set(&mut c, "trees.widths", Array::i64s(&[5], vec![15, 3, 3, 32, 16]));
    i64_scalar(&mut c, "trees.n_features", 69);
    c
}

/// The coverage snare: its net reads the flat channel only.
fn cover_snare() -> Container {
    golden::model("cover_snare")
}

fn set(c: &mut Container, name: &str, a: Array) {
    c.insert(name, a);
}

fn text(c: &mut Container, name: &str, s: &str) {
    c.insert(name, Array::text(s));
}

fn f64_scalar(c: &mut Container, name: &str, v: f64) {
    c.insert(name, Array::f64s(&[], vec![v]));
}

fn i64_scalar(c: &mut Container, name: &str, v: i64) {
    c.insert(name, Array::i64s(&[], vec![v]));
}

/// Copies net 0's weights as net `k`.
fn add_net(c: &mut Container, k: usize) {
    for p in NET_PARAMS {
        let a = c.get(&format!("net.0.{p}")).unwrap().clone();
        c.insert(&format!("net.{k}.{p}"), a);
    }
}

fn is_unknown(what: &'static str) -> impl Fn(&SpecError) -> bool {
    move |e| matches!(e, SpecError::Unknown { what: w, .. } if *w == what)
}

fn is_mismatch(what: &'static str) -> impl Fn(&SpecError) -> bool {
    move |e| matches!(e, SpecError::Mismatch { what: w, .. } if *w == what)
}

fn is_missing(name: &'static str) -> impl Fn(&SpecError) -> bool {
    move |e| matches!(e, SpecError::Container(ContainerError::Missing(n)) if n == name)
}

type Case = (&'static str, fn() -> Container, Box<dyn Fn(&mut Container)>, Box<dyn Fn(&SpecError) -> bool>);

/// One mutated container per refusal in section 4 of the design, each refused with its own named error.
/// Every case starts from a model that parses (a shipped model, the flat coverage snare, or the kick with
/// tmpl3), so the refusal is the mutation's.
#[test]
fn refuses_each_bad_entry() {
    let kick: fn() -> Container = || golden::model("kick");
    let snare: fn() -> Container = || golden::model("snare");
    let flat: fn() -> Container = cover_snare;
    let cover_kick: fn() -> Container = || golden::model("cover_kick");
    let tmpl3: fn() -> Container = kick_tmpl3;
    let cases: Vec<Case> = vec![
        ("contract missing", kick, Box::new(|c| drop(c.remove("contract"))), Box::new(|e| *e == SpecError::Contract(None))),
        ("contract 1", kick, Box::new(|c| set(c, "contract", Array::i32_scalar(1))), Box::new(|e| *e == SpecError::Contract(Some(1)))),
        ("contract 3", kick, Box::new(|c| set(c, "contract", Array::i32_scalar(3))), Box::new(|e| *e == SpecError::Contract(Some(3)))),
        ("unknown family", kick, Box::new(|c| text(c, "family", "clap")), Box::new(is_unknown("family"))),
        ("unknown finder kind", kick, Box::new(|c| text(c, "finder", "rise:6")), Box::new(is_unknown("finder"))),
        ("snare finder without a level", snare, Box::new(|c| text(c, "finder", "snare:loud")), Box::new(is_unknown("finder"))),
        ("snare finder level disagrees", snare, Box::new(|c| text(c, "finder", "snare:6")), Box::new(is_mismatch("finder"))),
        ("unknown measure", kick, Box::new(|c| text(c, "trees.measures", "f15,tail3,tmpl3,res16,low16")), Box::new(is_unknown("measure"))),
        ("unknown stage measure", kick, Box::new(|c| text(c, "stage.shape", "prof32,glide3")), Box::new(is_unknown("measure"))),
        ("unknown channel", flat, Box::new(|c| text(c, "net.extra", "side")), Box::new(is_unknown("channel"))),
        ("unknown combine", kick, Box::new(|c| text(c, "combine", "max")), Box::new(is_unknown("combine"))),
        ("unknown models", kick, Box::new(|c| text(c, "models", "net,trees,svm")), Box::new(is_unknown("models"))),
        (
            "f15 with the snare finder",
            snare,
            // A snare model whose stage reads f15: everything else consistent.
            Box::new(|c| text(c, "stage.shape", "f15")),
            Box::new(is_mismatch("f15")),
        ),
        ("combine disagrees with models", kick, Box::new(|c| text(c, "combine", "net")), Box::new(is_mismatch("combine"))),
        ("measure width", kick, Box::new(|c| set(c, "trees.widths", Array::i64s(&[7], vec![15, 3, 32, 16, 16, 13, 3]))), Box::new(is_mismatch("measure width"))),
        ("trees n_features", kick, Box::new(|c| i64_scalar(c, "trees.n_features", 67)), Box::new(is_mismatch("forest width"))),
        ("stage n_features", kick, Box::new(|c| i64_scalar(c, "stage.n_features", 4)), Box::new(is_mismatch("forest width"))),
        ("rate 44100", kick, Box::new(|c| i64_scalar(c, "rate", 44_100)), Box::new(is_mismatch("model grid"))),
        ("hop 512", kick, Box::new(|c| i64_scalar(c, "hop", 512)), Box::new(is_mismatch("model grid"))),
        ("a part's hop", kick, Box::new(|c| i64_scalar(c, "extra.hop", 128)), Box::new(is_mismatch("model grid"))),
        ("a part's sample rate", snare, Box::new(|c| i64_scalar(c, "net.sample_rate", 44_100)), Box::new(is_mismatch("model grid"))),
        ("no windows", kick, Box::new(|c| set(c, "stage.windows_s", Array::f64s(&[0], vec![]))), Box::new(is_mismatch("stage.windows_s"))),
        (
            "five windows",
            kick,
            Box::new(|c| set(c, "stage.windows_s", Array::f64s(&[5], vec![1.0, 2.0, 4.0, 8.0, 16.0]))),
            Box::new(is_mismatch("stage.windows_s")),
        ),
        ("level column outside its measure", kick, Box::new(|c| text(c, "stage.level", "f15:15")), Box::new(is_mismatch("stage.level"))),
        (
            "gate without cuts",
            kick,
            Box::new(|c| {
                c.remove("stage.cut_detector");
                c.remove("stage.cut_stage");
            }),
            Box::new(is_mismatch("stage cuts")),
        ),
        ("gate with one cut", kick, Box::new(|c| drop(c.remove("stage.cut_stage"))), Box::new(is_mismatch("stage cuts"))),
        ("cuts without a gate", kick, Box::new(|c| f64_scalar(c, "stage.gate", 0.0)), Box::new(is_mismatch("stage cuts"))),
        ("a negative gate", kick, Box::new(|c| f64_scalar(c, "stage.gate", -1.0)), Box::new(is_mismatch("stage.gate"))),
        ("tmpl3 without its templates", tmpl3, Box::new(|c| drop(c.remove("fit.tmpl3.high"))), Box::new(is_mismatch("fitted measure"))),
        ("tmpl3 spans disagree", tmpl3, Box::new(|c| i64_scalar(c, "fit.tmpl3.span", 7)), Box::new(is_mismatch("fitted measure"))),
        ("net.extra against net.channels", flat, Box::new(|c| text(c, "net.extra", "")), Box::new(is_mismatch("net channels"))),
        ("net.count 0", kick, Box::new(|c| i64_scalar(c, "net.count", 0)), Box::new(is_mismatch("net.count"))),
        ("net.count missing", kick, Box::new(|c| drop(c.remove("net.count"))), Box::new(is_missing("net.count"))),
        (
            "a missing weight of a counted net",
            kick,
            Box::new(|c| {
                i64_scalar(c, "net.count", 2);
                add_net(c, 1);
                c.remove("net.1.head.2.bias");
            }),
            Box::new(is_missing("net.1.head.2.bias")),
        ),
        ("weights past net.count", kick, Box::new(|c| add_net(c, 1)), Box::new(is_mismatch("net weights"))),
        (
            "nets with different shapes",
            kick,
            Box::new(|c| {
                i64_scalar(c, "net.count", 2);
                add_net(c, 1);
                c.insert("net.1.head.0.bias", Array { shape: vec![16], data: crate::trained::container::Data::F32(vec![0.0; 16]) });
            }),
            Box::new(is_mismatch("net weights")),
        ),
        ("an unknown entry under a known prefix", kick, Box::new(|c| f64_scalar(c, "stage.blend_clip", 1e-6)), Box::new(is_unknown("entry"))),
        ("an unknown top-level entry", kick, Box::new(|c| i32_entry(c, "seed")), Box::new(is_unknown("entry"))),
        ("a reserved part", kick, Box::new(|c| f64_scalar(c, "res.window_s", 0.4)), Box::new(is_unknown("entry"))),
        ("an unused part's entries", snare, Box::new(|c| f64_scalar(c, "base.eps", 1e-12)), Box::new(is_mismatch("entry"))),
        ("a flat entry without the flat channel", cover_kick, Box::new(|c| i64_scalar(c, "flat.n_fft", 2048)), Box::new(is_mismatch("entry"))),
        ("stage neither 0 nor 1", kick, Box::new(|c| set(c, "stage", Array::i32_scalar(2))), Box::new(is_mismatch("stage"))),
        ("a duplicate measure", kick, Box::new(|c| text(c, "stage.shape", "prof32,prof32")), Box::new(is_mismatch("measure list"))),
        ("family missing", kick, Box::new(|c| drop(c.remove("family"))), Box::new(is_missing("family"))),
    ];
    let mut failures = Vec::new();
    for (name, base, mutate, want) in &cases {
        let mut c = base();
        Spec::parse(&c).unwrap_or_else(|e| panic!("{name}: the unmutated model is refused: {e}"));
        mutate(&mut c);
        match Spec::parse(&c) {
            Ok(_) => failures.push(format!("{name}: accepted")),
            Err(e) if want(&e) => {}
            Err(e) => failures.push(format!("{name}: refused with another error: {e:?}")),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    println!("{} refusals", cases.len());
}

fn i32_entry(c: &mut Container, name: &str) {
    c.insert(name, Array::i32_scalar(1));
}

/// `runtime_parts.json` written from this build's registry, byte for byte in the file's layout (one part kind
/// per line; the file both sides check, D6).
fn runtime_parts() -> String {
    let list = |items: Vec<&str>| {
        let quoted: Vec<String> = items.iter().map(|s| format!("\"{s}\"")).collect();
        format!("[{}]", quoted.join(", "))
    };
    let measures: Vec<String> = Measure::ALL.iter().map(|m| format!("\"{}\": {}", m.name(), m.width())).collect();
    format!(
        "{{\n  \"finders\": {},\n  \"measures\": {{{}}},\n  \"channels\": {},\n  \"combines\": {},\n  \"nets\": {{\"min_count\": {MIN_NETS}}},\n  \"stage\": {{\"max_windows\": {MAX_WINDOWS}, \"level\": true, \"gate\": true}}\n}}\n",
        list(FinderKind::ALL.iter().map(|f| f.name()).collect()),
        measures.join(", "),
        list(Channel::ALL.iter().map(|c| c.name()).collect()),
        list(Combine::ALL.iter().map(|c| c.name()).collect()),
    )
}

#[test]
fn registry_matches_runtime_parts() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/detectors/runtime_parts.json");
    let file = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(file, runtime_parts(), "runtime_parts.json differs from this build's parts");
}
