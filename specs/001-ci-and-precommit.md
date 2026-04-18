# Spec 001 — CI and Pre-commit

## Version history

| Version | Date       | Author      | Changes        |
| ------- | ---------- | ----------- | -------------- |
| 0.1     | 2026-04-18 | bf3 (agent) | Initial draft. |

| Field    | Value                                                         |
| -------- | ------------------------------------------------------------- |
| Tracking | DPU-7 (parent: DPU-4 — EPIC InFMon)                           |
| Repo     | https://github.com/r12f/InFMon                                |

## 1. Motivation

Before a single line of production code lands, InFMon needs a deterministic,
reproducible quality gate. The project mixes three toolchains (Rust, C/C++, and
shell/markdown), targets an unusual platform (BlueField-3 ARM cores), and
depends on VPP — a heavyweight runtime that is awkward to install on developer
laptops. Without a written CI contract, every contributor will paper over the
gaps differently and the codebase will rot before it ships.

This spec defines:

- The local pre-commit contract every developer (and agent) MUST satisfy.
- The GitHub Actions jobs that gate every PR.
- Which checks are deliberately **out of scope** for CI and why.
- The branch protection configuration that turns the above into an enforceable
  rule.
- The container image and caching choices that keep CI fast and stable.

## 2. Scope

In scope:

- Pre-commit hook configuration (`.pre-commit-config.yaml`) and per-tool
  configuration files (`rustfmt.toml`, `.clang-format`, `cppcheck` suppressions,
  `.markdownlint.yaml`, `commitlint.config.js`).
- GitHub Actions workflows under `.github/workflows/`:
  - `lint.yml`
  - `rust-test.yml`
  - `cpp-test.yml`
  - `cross-build.yml`
- Container image selection for the C/C++ side (VPP available).
- Caching strategy for `cargo`, `ccache`, and apt packages.
- Branch protection and required-status-check configuration on `main`.

Out of scope (tracked separately):

- E2E / real-packet replay tests (run manually on `r12f-bf3`; see §6).
- Release / publishing workflows (separate spec, post-MVP).
- Code coverage gating (deferred; we will collect, not enforce, in v1).
- Fuzzing, ASan/TSan rotations (post-MVP spec).

## 3. Pre-commit hooks

We use [pre-commit](https://pre-commit.com/) as the local orchestrator. Hooks
are checked into `.pre-commit-config.yaml` and pinned by `rev`. The same hooks
run in the `lint` GH Action (§4.1) so local and CI verdicts agree.

| Hook            | Purpose                                                  | Failure = block |
| --------------- | -------------------------------------------------------- | --------------- |
| `rustfmt`       | Format Rust sources (`cargo fmt --all -- --check`)       | Yes             |
| `clippy`        | Lint Rust (`cargo clippy --all-targets -- -D warnings`)  | Yes             |
| `clang-format`  | Format C/C++ sources (style: `file`, `.clang-format`)    | Yes             |
| `cppcheck`      | Static analysis on C/C++ (`--enable=warning,performance,portability --error-exitcode=1`) | Yes |
| `markdownlint`  | Lint markdown (specs, READMEs); config `.markdownlint.yaml` | Yes          |
| `commitlint`    | Conventional Commits on commit messages                  | Yes             |
| `trailing-whitespace`, `end-of-file-fixer`, `check-yaml`, `check-merge-conflict` | Hygiene | Yes |

Notes:

- `clippy` runs in pre-commit using `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
- `cppcheck` runs only on changed C/C++ files in the local hook (fast); the CI
  job runs it across the full tree.
- `commitlint` enforces [Conventional Commits](https://www.conventionalcommits.org/);
  scope list seeded with `backend`, `frontend`, `cli`, `tests`, `ci`, `specs`.
- All commits require a `Signed-off-by:` trailer (DCO). Enforced by a
  `pre-commit` local hook plus a CI job (DCO bot or equivalent).

`make lint` MUST be a thin wrapper around `pre-commit run --all-files` so
contributors have one command to memorise.

## 4. GitHub Actions

Workflows live under `.github/workflows/`. All workflows trigger on
`pull_request` (against `main`) and on `push` to `main`. The matrix is kept
minimal in v1; expand only when there's a concrete bug it would have caught.

### 4.1 `lint.yml`

- Runner: `ubuntu-22.04`.
- Steps:
  1. `actions/checkout@v4`.
  2. Install Rust toolchain (stable, with `rustfmt`, `clippy`).
  3. Install `pre-commit` via `pipx`.
  4. `pre-commit run --all-files --show-diff-on-failure`.
- Cache: `~/.cache/pre-commit` keyed on `.pre-commit-config.yaml`.

### 4.2 `rust-test.yml`

- Runner: `ubuntu-22.04`.
- Toolchain: stable Rust (pinned in `rust-toolchain.toml`).
- Steps:
  1. Checkout, install toolchain.
  2. `cargo build --workspace --all-targets --locked`.
  3. `cargo test --workspace --all-features --locked`.
  4. `cargo doc --workspace --no-deps` (catches broken intra-doc links).
- Cache: `Swatinem/rust-cache@v2` (registry, git index, `target/`).
- Note: `infmon-frontend` and `infmon-cli` build cleanly without VPP headers.
  Anything that needs VPP at compile time MUST be feature-gated and excluded
  from this job (covered in `cpp-test.yml`).

### 4.3 `cpp-test.yml`

- Runner: `ubuntu-22.04` host, container `ligato/vpp-base:24.02` (see §5).
- Steps:
  1. Checkout.
  2. Install build deps already in image: `cmake`, `ninja-build`, `g++`,
     `clang`, `libgtest-dev`, `ccache`, `pkg-config`. Verify VPP dev headers
     present at `/usr/include/vpp/`.
  3. Configure: `cmake -S infmon-backend -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug -DCMAKE_C_COMPILER_LAUNCHER=ccache -DCMAKE_CXX_COMPILER_LAUNCHER=ccache`.
  4. Build: `cmake --build build`.
  5. Test: `ctest --test-dir build --output-on-failure`.
- Cache:
  - `~/.ccache` keyed on `${{ runner.os }}-ccache-${{ hashFiles('infmon-backend/**/CMakeLists.txt') }}`.
  - apt cache via `actions/cache` if any extra packages are installed.

### 4.4 `cross-build.yml`

Cross-compilation smoke test: prove the BF-3 target builds; do not run tests
(no aarch64 emulator with VPP in CI).

- Runner: `ubuntu-22.04`.
- Targets:
  - Rust: `aarch64-unknown-linux-gnu` via `cargo build --workspace --target aarch64-unknown-linux-gnu --release`. Linker: `aarch64-linux-gnu-gcc` from `gcc-aarch64-linux-gnu`. `cross` is acceptable as an alternative.
  - C/C++: cross-toolchain in container `ligato/vpp-base:24.02-arm64` (multi-arch tag) or `--platform linux/arm64` build via `docker buildx`. Compile only; do **not** invoke `ctest`.
- Cache: `Swatinem/rust-cache@v2` with target key suffix; `~/.ccache`.
- Failure modes worth calling out:
  - Missing VPP aarch64 headers → fail loudly with a hint to update §5.
  - Linker mismatch → fail with a hint to set `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER`.

### 4.5 DCO / commit-message check

- Reuses `lint.yml` step or a dedicated job using
  `actions/dco` / `commitlint-github-action`. Either is acceptable; pick one
  in implementation.

## 5. Container image for VPP-in-CI

**Proposal: use `ligato/vpp-base:24.02` (Debian-based, VPP 24.02 LTS) as the
primary CI image for `cpp-test` and `cross-build`.**

Reasoning:

- fd.io publishes source and packages but no general-purpose multi-arch dev
  image with VPP headers preinstalled. Their `csit` images are CI-flavoured
  and brittle outside their pipelines.
- `ligato/vpp-base` tracks fd.io's LTS releases, ships the `vpp-dev` package
  (headers under `/usr/include/vpp/`), is published for both `amd64` and
  `arm64`, and is small enough (<400 MB) for GH-hosted runners.
- It is community-maintained but actively updated; we pin by digest in the
  workflow to avoid surprise rebases.

Acceptance criteria for the image:

1. Provides VPP headers and `libvppinfra-dev` for the target VPP LTS.
2. Available for `linux/amd64` and `linux/arm64`.
3. Pinned by digest in workflows.
4. License compatible with Apache-2.0 (Apache-2.0 itself).

If `ligato/vpp-base` becomes unmaintained, fallback is a thin Dockerfile in
`ci/images/vpp-dev/` built from `debian:bookworm-slim` + fd.io apt repo,
published to `ghcr.io/r12f/infmon-vpp-dev`. This fallback path is documented
but not built until needed.

## 6. E2E tests — explicitly NOT in CI

E2E tests live under `tests/` and replay real packet captures against a
running VPP instance with the InFMon plugin loaded. They require:

- A BlueField-3 card (or compatible DPDK NIC) with SR-IOV configured.
- Kernel modules and hugepages set up out-of-band.
- 5–15 minutes of wall time per scenario.

These are unsuitable for GH-hosted runners. They run manually on `r12f-bf3`,
the bench machine, via `make e2e`. A lightweight nightly job (future spec)
may SSH into `r12f-bf3` as a self-hosted runner; that is **out of scope here**.

CI MUST refuse to silently skip E2E. The `tests/` directory has its own
`README.md` documenting the manual workflow, and `make test` in the repo root
prints a banner explaining that E2E is run separately.

## 7. Branch protection on `main`

Configured via repo settings (or `gh api` script under `ci/branch-protection.sh`):

- Require pull request reviews before merging:
  - **1 approving review** required.
  - **Require review from Code Owners** (`.github/CODEOWNERS` lists @banidoru as
    owner of `/`). Per the EPIC, @banidoru's sign-off is required before merge.
  - Dismiss stale reviews on new commits.
- Require status checks to pass before merging. Required checks:
  - `lint`
  - `rust-test`
  - `cpp-test`
  - `cross-build`
  - `dco`
- Require branches to be up to date before merging.
- Require signed commits: **off** in v1 (DCO sign-off is enforced instead;
  signed commits raise contributor friction without commensurate benefit at
  this stage).
- Require linear history: **on**.
- Restrict who can push to matching branches: maintainers only.
- Allow force pushes: **off**. Allow deletions: **off**.

## 8. Caching strategy

| Cache                | Key                                                                          | Restore keys                  |
| -------------------- | ---------------------------------------------------------------------------- | ----------------------------- |
| Cargo (registry/git) | `${{ runner.os }}-cargo-${{ hashFiles('**/Cargo.lock') }}`                   | `${{ runner.os }}-cargo-`     |
| Cargo `target/`      | Managed by `Swatinem/rust-cache@v2` (per-job, per-target)                    | (built-in)                    |
| `ccache`             | `${{ runner.os }}-ccache-${{ hashFiles('infmon-backend/**/CMakeLists.txt') }}` | `${{ runner.os }}-ccache-`  |
| `pre-commit`         | `${{ runner.os }}-precommit-${{ hashFiles('.pre-commit-config.yaml') }}`     | `${{ runner.os }}-precommit-` |
| apt (if used)        | `${{ runner.os }}-apt-${{ hashFiles('ci/apt-packages.txt') }}`               | `${{ runner.os }}-apt-`       |

Targets: warm CI (full cache hit) ≤ 4 min for `lint`, ≤ 6 min for `rust-test`,
≤ 10 min for `cpp-test`, ≤ 8 min for `cross-build`. Cold runs may take ~2x.

## 9. Open questions

1. **Self-hosted runner on `r12f-bf3`?** Would unlock optional E2E in CI.
   Defer until v1 ships and we feel the pain.
2. **Pin VPP LTS to 24.02 vs. 24.06?** 24.02 is current LTS at spec time;
   confirm with @banidoru before implementation.
3. **Use `cross` vs. raw cross-compiler for Rust aarch64?** Both work;
   defaulting to raw cross-compiler keeps the dependency surface smaller.

## 10. Acceptance

This spec is accepted when:

- @banidoru signs off on the PR.
- The "Open questions" in §9 are either resolved inline or split into
  follow-up issues.

Implementation lands as a follow-up PR per the spec-first workflow described
in DPU-4.
