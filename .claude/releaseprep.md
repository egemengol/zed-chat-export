# Publishing zed-chat-export as Quality Open Source

## Context

`zed-chat-export` is a mature, working CLI tool that exports Zed AI chat conversations to Markdown. The code quality is high, the feature set is solid, and it has a clear niche audience (Zed users). However, it currently has: no user-facing README, no tests, no CI/CD, no GitHub remote, and no release infrastructure. This plan covers everything needed to publish it as a quality open source project.

---

## Current State Summary

- Binary: `zed-chat-export`
- Language: Rust
- License: AGPL-3.0-or-later
- Platforms: macOS primary; Linux should work; Windows uncertain
- CLI: flat flags, no subcommands, config file support
- Code quality: high, well-commented internals
- Missing: everything user-facing

---

## What a Quality Open Source Release Needs

### 1. Repository & GitHub Setup

- Push to GitHub (remote doesn't exist yet)
- Repository description, topics/tags (e.g. `zed`, `markdown`, `cli`, `rust`)
- Social preview image (optional but adds polish)

### 2. README

This is the most critical deliverable. Needs:

- Hook: what it does and why you'd want it (one paragraph, maybe a gif/screenshot)
- Install section: multiple methods (see Distribution below)
- Usage: CLI help output + a few practical examples
- Config file: document `config.toml` fields
- Output format: show what a generated Markdown file looks like
- Platform support table (macOS/Linux/Windows)
- How it works: brief explanation of the Zed DB + Zstd pipeline (you have this in `importer.rs`)
- Privacy note: read-only access, no data sent anywhere
- Limitations: what versions of Zed are supported, known edge cases
- Contributing section
- License badge + license section

### 3. Changelog

- `CHANGELOG.md` following [Keep a Changelog](https://keepachangelog.com/) format
- Establish a version (currently no version in Cargo.toml beyond `0.1.0` — decision needed)
- Document what the initial release includes

### 4. Contributing Guide (`CONTRIBUTING.md`)

- How to build locally
- How to run tests (once they exist)
- How to test with a real `threads.db`
- Code style / `clippy` / `rustfmt` expectations
- How to submit issues and PRs

### 5. GitHub Infrastructure (`.github/`)

- **Issue templates**: bug report, feature request
- **PR template**: checklist (tests added, docs updated, etc.)
- **`SECURITY.md`**: how to report security vulnerabilities

### 6. CI/CD (GitHub Actions)

Three workflows to consider:

- **`ci.yml`**: On every push/PR — `cargo fmt --check`, `cargo clippy`, `cargo test`, build on matrix (macOS + Linux, possibly Windows)
- **`release.yml`**: On `v*` tags — cross-compile for all targets, attach binaries to GitHub Release
- **`deny.yml`** (optional): `cargo-deny` for license and advisory checks

### 7. Cross-Platform Builds & Distribution

Multiple distribution channels to decide on:

**a) GitHub Releases (pre-built binaries)**
Targets to consider:

- `x86_64-apple-darwin` (macOS Intel)
- `aarch64-apple-darwin` (macOS Apple Silicon)
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `x86_64-pc-windows-msvc` (if Zed supports Windows sufficiently)

Tool: `cross` or native runners + `cargo-dist`

**b) `cargo install`** via crates.io

- Needs `cargo publish` setup
- Verify all metadata in `Cargo.toml` (description, keywords, categories, etc.)

**c) Homebrew**

- A `homebrew-tap` repository (e.g. `egemengol/homebrew-tap`)
- Formula that downloads the macOS binary from GitHub Releases
- Simple one-liner install: `brew install egemengol/tap/zed-chat-export`

**d) Shell install script** (optional)

- A `install.sh` that detects platform and downloads the right binary

**e) `mise` / `aqua` / `cargo-binstall`** (optional, later)

### 8. Tests

Currently zero tests. For a publishable project:

- Unit tests in `utils.rs` (decompress, parse_frontmatter, filename generation)
- Unit tests in `renderer.rs` (frontmatter rendering, message formatting)
- Integration test: a fixture `threads.db` with known content → assert expected Markdown output
- The `scratch/threads.db` could be anonymized/trimmed as a test fixture

### 9. Examples

- An `examples/` directory or `docs/examples/` with sample output Markdown
- Show what a typical export looks like without exposing real user data

### 10. Code Quality Gates

- `rustfmt.toml` (or just rely on defaults)
- `clippy.toml` or `#![deny(clippy::...)]` in `main.rs`
- MSRV (Minimum Supported Rust Version) — declare and test it
- `cargo-deny` for dependency auditing

### 11. Versioning & Releases

- Decide on a versioning strategy (semver: is this `0.1.0`, `1.0.0`?)
- Tag releases consistently (`v0.1.0`)
- GitHub Release notes (generated from CHANGELOG or via `release-drafter`)

### 12. Zed-Specific Compatibility Notes

- The tool tracks Zed's internal DB schema — this is a moving target
- Need a policy for what happens when Zed updates its schema
- Consider documenting which Zed version(s) are tested/supported
- Maybe a `ZED_COMPAT.md` or a section in README

---

## Decision Points Needing Your Input

Before execution, these need answers:

1. **Initial version number**: `0.1.0` (alpha/beta signal) vs `1.0.0` (stable signal)?
2. **Windows support**: Include Windows targets in CI/release, or explicitly not supported?
3. **crates.io publishing**: Do you want to publish to crates.io, or only GitHub Releases + Homebrew?
4. **Tests before release**: Block release on writing tests, or ship now and add tests after?
5. **Homebrew tap**: Create a separate `homebrew-tap` repo, or skip for now?
6. **Shell install script**: Worth adding, or just `cargo install` and `brew`?
7. **`cargo-dist`**: Use it for release automation (handles cross-compilation + installers), or roll your own GitHub Actions?
8. **GitHub username / org**: Is `egemengol` the correct GitHub username for the repo URL in Cargo.toml?
9. **Code of Conduct**: Include one (e.g. Contributor Covenant), or skip?
10. **Minimum Supported Rust Version (MSRV)**: Do you want to declare one?

---

## Suggested Execution Order (once decisions are made)

1. Cargo.toml metadata cleanup
2. README (the most impactful single thing)
3. CHANGELOG
4. GitHub repo setup + push
5. CI workflow (fmt + clippy + test)
6. Release workflow + cross-compiled binaries
7. CONTRIBUTING + issue templates
8. Tests (unit + integration fixture)
9. crates.io publish (if desired)
10. Homebrew tap (if desired)
