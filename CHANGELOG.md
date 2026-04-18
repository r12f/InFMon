# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial repository scaffolding.
- Apache-2.0 `LICENSE`.
- `README.md` with high-level project description and link to `specs/`.
- `CODEOWNERS` requiring review from `@banidoru` on all paths.
- Top-level layout: `specs/`, `src/{infmon-backend,infmon-frontend,infmon-cli}/`, `tests/`.
- `.gitignore` for Rust, C/C++, and VPP build artifacts.
- This `CHANGELOG.md` (Keep a Changelog format).
- Debian packaging tree (`debian/`) producing three binary `.deb`s
  (`infmon-backend`, `infmon-frontend`, `infmon-cli`) plus an `infmon`
  meta package, per spec 008.
- systemd unit `infmon-frontend.service` (Type=notify, hardened,
  `PartOf=vpp.service`).
- Maintainer scripts that strictly leave VPP alone on install,
  upgrade, remove, and purge.
- `tools/sync-debian-changelog.py`: regenerates `debian/changelog`
  from this file at source-package build time.
- GitHub Actions workflow `package-deb.yml`: builds the `.deb`s
  for `arm64` on every push and uploads them as workflow artefacts.

<!-- TODO: replace with a real diff link (e.g. compare/v0.1.0...HEAD) once the
     first version is tagged. `compare/HEAD...HEAD` would always render an
     empty diff, so it is intentionally omitted here. -->

[Unreleased]: https://github.com/r12f/InFMon/commits/main
