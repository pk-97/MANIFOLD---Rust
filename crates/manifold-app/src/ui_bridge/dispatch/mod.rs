//! Inspector dispatch handlers, split by domain (UI_FUNNEL_DECOMPOSITION P-B,
//! D6). Each module owns a disjoint slice of the inspector's `PanelAction`
//! variants, speaks today's `PanelAction`, and reads `ctx` fields directly.
//! `inspector::dispatch_inspector` is an ordered first-non-unhandled CHAIN over
//! these modules — NO per-variant delegation arm table (a misroute would hide
//! there). Bridge layer (D2): intent → `ContentCommand`/`EditingService`.

pub(crate) mod browser;

#[cfg(test)]
mod chain_completeness {
    //! INV (P-B D6 companion): chain membership is semantics the scaffold
    //! allowlist deliberately can't see and the variant census can't reach
    //! (it counts arms in files, not chain reachability). A handler module on
    //! disk but absent from the router chain makes its variants silently
    //! unhandled. Source-scan (like `no_bespoke_row_infra`): the router must
    //! contain EXACTLY one `…::dispatch_<m>(action, ctx)` chain call per handler
    //! module file in `ui_bridge/dispatch/` (mod.rs/resolve.rs exempt), and no
    //! chain call without a file.
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    /// Extract `<module>` from every `::dispatch_<module>(action, ctx)` chain
    /// call, asserting the delegated handler name matches its path segment.
    fn chained_modules(router_src: &str) -> BTreeSet<String> {
        const MARK: &str = "::dispatch_";
        let mut out = BTreeSet::new();
        for (i, _) in router_src.match_indices(MARK) {
            let after = &router_src[i + MARK.len()..];
            let handler: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let call = &after[handler.len()..];
            if !call.starts_with("(action, ctx)") {
                continue; // a helper/definition, not a chain call
            }
            // the module path segment immediately before `::dispatch_`
            let before = &router_src[..i];
            let module: String = before
                .chars()
                .rev()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            assert_eq!(
                module, handler,
                "chain call ::dispatch_{handler}(action, ctx) is reached through module `{module}` — name mismatch is a misroute"
            );
            assert!(out.insert(module.clone()), "module `{module}` is chained more than once");
        }
        out
    }

    #[test]
    fn dispatch_chain_completeness() {
        let ui_bridge = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui_bridge");

        let mut files = BTreeSet::new();
        for entry in fs::read_dir(ui_bridge.join("dispatch")).expect("dispatch dir") {
            let p = entry.expect("dir entry").path();
            if p.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let stem = p.file_stem().unwrap().to_str().unwrap().to_string();
            if stem != "mod" && stem != "resolve" {
                files.insert(stem);
            }
        }

        let router = fs::read_to_string(ui_bridge.join("inspector.rs")).expect("inspector.rs");
        let chained = chained_modules(&router);

        assert_eq!(
            files, chained,
            "every ui_bridge/dispatch/ handler module must be chained exactly once by dispatch_inspector \
             (on disk but not chained = silently unhandled variants; chained but no file = dangling call)"
        );
    }
}
