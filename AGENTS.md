## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues for this repository. See `docs/agents/issue-tracker.md`.

### Domain docs

Single-context: root `GLOSSARY.md` and `docs/adr/`. See `docs/agents/domain.md`.

## Coding standards

Rust code MUST follow CODING_STANDARDS.md.

### Rust unit tests

Unit tests MUST live in crate-local `unit-tests/`, mirroring `src/`, loaded through `mod tests` with `#[cfg(test)]` and a relative `#[path]`. Integration tests stay in `tests/`.

### README

README.md is a short, user-facing homepage. MUST NOT add developer notes, implementation details, or walkthroughs unless explicitly requested.
