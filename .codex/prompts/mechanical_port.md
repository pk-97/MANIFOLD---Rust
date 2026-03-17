# Mechanical Port Task Frame

Use this when implementing a direct Unity-to-Rust translation.

## Required Inputs

- Unity source file
- Target Rust file(s)
- Relevant registry or shader files if the target is an effect or generator

## Execution Checklist

1. Read Unity source completely
2. Map fields, methods, traits, base classes, dependencies
3. Translate line by line
4. Re-check constants, param indices, defaults, texture formats, and math ops
5. Validate the narrowest affected crate
6. Update tracking docs if status changed

## Hard Stops

- Do not synthesize from audit prose
- Do not flatten architecture
- Do not change values for imagined platform reasons
- Do not leave stale inline copies behind if the new service replaces them
