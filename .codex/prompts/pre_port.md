# Pre-Port Task Frame

Use this when preparing to port or verify a Unity file.

## Inputs

- Unity source path or class name
- Intended Rust crate/module

## Output

Produce:
1. Current status from `docs/PORT_STATUS.md`
2. Dependency list
3. Missing prerequisite ports
4. Existing inline copies to consolidate
5. Recommended port order
6. Narrowest useful validation command after editing

## Reminders

- Read the Unity file completely first
- Search `app.rs` and `ui_bridge.rs` for scattered logic
- Check `docs/KNOWN_DIVERGENCES.md` before assuming a difference is required
