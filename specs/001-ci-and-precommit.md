# Spec 001 — CI and Pre-commit

## Version history

| Version | Date       | Author       | Changes |
| ------- | ---------- | ------------ | ------- |
| 0.1     | 2026-04-18 | Riff (r12f)  | Initial draft. Defines local pre-commit contract, GH Actions workflows, x86 vs aarch64 split, and the §11 changelog of review fixes from PR #4 (banidoru). |

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
- The top-level `Makefile` target contract that wraps both (so contributors
  have one entry point per check; see §3.1).
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
| `clippy`        | Lint Rust (`cargo clippy --workspace --all-targets --all-features -- -D warnings`)  | Yes             |
| `clang-format`  | Format C/C++ sources (style: `file`, `.clang-format`)    | Yes             |
| `cppcheck`      | Static analysis on C/C++ (`--enable=warning,performance,portability --error-exitcode=1`) | Yes |
| `markdownlint`  | Lint markdown (specs, READMEs); config `.markdownlint.yaml` | Yes          |
| `commitlint`    | Conventional Commits on commit messages                  | Yes             |
| `trailing-whitespace`, `end-of-file-fixer`, `check-yaml`, `check-merge-conflict` | Hygiene | Yes |

Notes:

- `clippy` runs in pre-commit using
  `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  (same form as the table; `--all-features` matters because §4.2
  feature-gates VPP-dependent code).
- `cppcheck` runs only on changed C/C++ files in the local hook (fast); the CI
  job runs it across the full tree. **Tradeoff**: local pre-commit can pass while
  CI fails on untouched files (e.g. when a header change exposes a latent issue
  elsewhere). A `make cppcheck-full` target MUST be provided so contributors can
  reproduce the CI invocation locally on demand.
- `commitlint` enforces [Conventional Commits](https://www.conventionalcommits.org/);
  scope list seeded with `backend`, `frontend`, `cli`, `tests`, `ci`, `specs`.
- All commits require a `Signed-off-by:` trailer (DCO). Enforced by a
  `pre-commit` local hook plus a CI job (DCO bot or equivalent).

`make lint` MUST be a thin wrapper around `pre-commit run --all-files` so
contributors have one command to memorise.

### 3.1 Top-level `Makefile` target contract

The repo root MUST expose the following `make` targets (semantics fixed here so
implementers and CI agree):

| Target            | Semantics                                                                 |
| ----------------- | ------------------------------------------------------------------------- |
| `make lint`       | `pre-commit run --all-files --show-diff-on-failure` (same as `lint.yml`). |
| `make cppcheck-full` | Run `cppcheck` over the full C/C++ tree (matches the `cpp-test.yml` step). |
| `make test`       | Run all unit tests *that are safe in CI*: `cargo test --workspace --all-features --locked` plus `ctest --test-dir build` if `build/` exists. Prints a banner that E2E is run separately (see §6). |
| `make e2e`        | Run E2E suite under `tests/`. **Not** invoked by CI; requires `r12f-bf3` (see §6). |
| `make build`      | `cargo build --workspace --all-targets --locked` plus a `cmake --build build` if configured. Convenience for local dev. |
| `make clean`      | Remove `target/`, `build/`, and any other generated artefacts.            |

These names appear in §3, §4, and §6 — fixing them here avoids implementers
reverse-engineering them from scattered references.

## 4. GitHub Actions

Workflows live under `.github/workflows/`. All workflows trigger on
`pull_request` (against `main`) and on `push` to `main`. The matrix is kept
minimal in v1; expand only when there's a concrete bug it would have caught.

**Cross-cutting workflow contract** (applies to every workflow under
`.github/workflows/`):

- `permissions:` MUST be declared explicitly at the workflow level with the
  minimum needed set. Default for all v1 workflows is
  `permissions: { contents: read }`. Workflows that need more (e.g. to push a
  mirror image, comment on PRs, or write check runs) declare exactly those
  scopes and nothing else. This matters today for defense-in-depth and is a
  prerequisite for ever accepting fork PRs.
- `concurrency:` MUST be set so redundant runs on rapid pushes are cancelled:

  ```yaml
  concurrency:
    group: ${{ github.workflow }}-${{ github.ref }}
    cancel-in-progress: true
  ```

  This is part of the spec contract, not implementation discretion. (For the
  protected `main` branch we still want every push built; the
  `cancel-in-progress` behaviour only meaningfully fires on PR branches where
  rapid iteration is normal.)
- Default runner is `ubuntu-24.04` (Noble). Jammy reaches end of standard
  support April 2027; starting on Noble gives a longer runway and newer
  toolchains. Sections below say `ubuntu-24.04` for the same reason.

### 4.1 `lint.yml`

- Runner: `ubuntu-24.04`.
- Steps:
  1. `actions/checkout@v4`.
  2. Install Rust toolchain (stable, with `rustfmt`, `clippy`).
  3. Install `pre-commit` via `pipx`.
  4. `pre-commit run --all-files --show-diff-on-failure`.
- Cache: `~/.cache/pre-commit` keyed on `.pre-commit-config.yaml`.

### 4.2 `rust-test.yml`

- Runner: `ubuntu-24.04`.
- Toolchain: stable Rust (pinned in `rust-toolchain.toml`).
- Steps:
  1. Checkout, install toolchain.
  2. `cargo build --workspace --all-targets --locked`.
  3. `cargo test --workspace --all-features --locked`.
  4. `cargo doc --workspace --no-deps` (catches broken intra-doc links).
- Cache: `Swatinem/rust-cache@v2` (registry, git index, `target/`).
- Note: `infmon-frontend` and `infmon-cli` build cleanly without VPP headers.
  Anything that needs VPP at compile time MUST be feature-gated and excluded
  from this job (covered in `cpp-test.yml`). The exact feature-flag name and
  the precise `cargo` invocation that excludes it (e.g. `--no-default-features`
  vs. `--exclude infmon-backend` vs. a specific `--features` set) are
  **deliberately deferred to the workspace-layout spec / impl PR** because they
  depend on whether `infmon-backend` is a workspace member, an FFI shim crate,
  or out-of-tree. Tracked as Open Question §9.4.

### 4.3 `cpp-test.yml`

- Runner: `ubuntu-24.04` host, container `ligato/vpp-base:24.02` (see §5).
- Steps:
  1. Checkout.
  2. The base image already provides `cmake`, `ninja-build`, `g++`, `clang`,
     `libgtest-dev`, `ccache`, `pkg-config`, and the VPP dev headers under
     `/usr/include/vpp/`. The job MUST `command -v` / `dpkg -s`-verify these
     are present (fail fast with a clear message if the image drifts) and
     MUST NOT `apt-get install` them on every run. If a future need adds a
     package not in the image, install it explicitly here and pin in §5.
  3. Configure: `cmake -S infmon-backend -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug -DCMAKE_C_COMPILER_LAUNCHER=ccache -DCMAKE_CXX_COMPILER_LAUNCHER=ccache`.
  4. Build: `cmake --build build`.
  5. Test: `ctest --test-dir build --output-on-failure`.
- Cache:
  - `~/.ccache` keyed on `${{ runner.os }}-ccache-${{ hashFiles('infmon-backend/**/CMakeLists.txt') }}`.
  - apt cache via `actions/cache` if any extra packages are installed.

### 4.4 `cross-build.yml`

Cross-compilation smoke test: prove the BF-3 target builds; do not run tests
(no aarch64 emulator with VPP in CI).

- Runner: `ubuntu-24.04`.
- Targets:
  - Rust: `aarch64-unknown-linux-gnu` via
    `cargo build --workspace --target aarch64-unknown-linux-gnu --release`.
    Linker: `aarch64-linux-gnu-gcc` from `gcc-aarch64-linux-gnu`.
    `cross` is acceptable as an alternative.
  - C/C++: cross-toolchain in container
    `ligato/vpp-base:24.02-arm64` (multi-arch tag) **strongly preferred**
    over `--platform linux/arm64` build via `docker buildx`. The
    buildx/QEMU path emulates every compiler invocation and a full backend
    build can blow well past the §8 8-min warm target. Compile only;
    do **not** invoke `ctest`.
  - **Time budget for the QEMU fallback path**: if QEMU buildx is used,
    the spec accepts up to **20 min wall-time** for `cross-build`
    (vs. 8 min for the native cross-toolchain path) — implementers
    SHOULD treat anything beyond that as a CI bug and switch to the
    cross-toolchain image.
- Cache: `Swatinem/rust-cache@v2` with target key suffix; `~/.ccache`.
- Failure modes worth calling out:
  - Missing VPP aarch64 headers → fail loudly with a hint to update §5.
  - Linker mismatch → fail with a hint to set `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER`.

### 4.5 DCO / commit-message check

- A dedicated workflow (or a job inside `lint.yml`) MUST publish a required
  status check named exactly **`dco`** (lowercase, no suffix). The branch
  protection contract in §7 lists `dco` as a required check, so the check
  name is part of the spec contract — implementers may use
  `actions/dco@v1` or `commitlint-github-action`, but if the chosen tool's
  default check name differs, the workflow MUST rename it (via job `name:` or
  a wrapping `check_run`) so the protected name `dco` is what GitHub
  records. Picking the wrong tool without renaming silently breaks branch
  protection — call this out in the impl PR review.

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
published to `ghcr.io/r12f/infmon-vpp-dev`.

Additionally — independent of upstream maintenance status — the
implementation MUST mirror the pinned `ligato/vpp-base:24.02` digest to
`ghcr.io/r12f/infmon-vpp-dev` from day one (via a scheduled weekly job or a
one-shot `ci/mirror-image.sh`). CI consumes the `ghcr.io` mirror, not Docker
Hub, so a Docker Hub outage or rate-limit hit doesn't hard-block PRs. The
self-built fallback Dockerfile remains documented but is only built once
upstream actually goes stale.

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
  - Dismiss stale reviews on new commits. **Iteration-speed callout**:
    combined with the required CODEOWNERS review from @banidoru, every new
    push (including fixups and force-with-lease updates) re-arms the review
    requirement. This is the correct security default but does slow review
    iteration; reviewers and authors should expect a re-approval round trip
    on each push.
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
4. **VPP feature-flag name and `cargo` exclusion form for `rust-test.yml`?**
   Pinned to the workspace-layout spec / impl PR — depends on whether
   `infmon-backend` is a Cargo workspace member, an FFI shim crate, or
   out-of-tree. Surface the resolved choice back into §4.2 once known.

## 10. Acceptance

This spec is accepted when:

- @banidoru signs off on the PR.
- The "Open questions" in §9 are either resolved inline or split into
  follow-up issues.
- **Measurable implementation gate** (checked on the follow-up impl PR, not
  this spec PR): all five required workflows — `lint`, `rust-test`,
  `cpp-test`, `cross-build`, `dco` — pass green on a PR against `main` that
  contains a non-trivial change in each affected language (Rust, C/C++,
  markdown). A spec that "looks right" but whose implementation can't pass
  its own gate is not done.

Implementation lands as a follow-up PR per the spec-first workflow described
in DPU-4.

## 11. Changelog (review-driven, v0.2)

The following changes were made in response to PR #4 review (banidoru):

- §2 scope: added `Makefile` target contract bullet pointing to §3.1.
- §3 hooks table: clippy invocation aligned to the canonical
  `--workspace --all-targets --all-features` form (matches the notes).
- §3 notes: documented the local-vs-CI cppcheck tradeoff and added the
  required `make cppcheck-full` escape hatch.
- §3.1 (new): top-level `Makefile` target contract (`lint`, `cppcheck-full`,
  `test`, `e2e`, `build`, `clean`).
- §4 preamble (new): cross-cutting workflow contract requires explicit
  `permissions: { contents: read }` and `concurrency:` with
  `cancel-in-progress`. Default runner moved to `ubuntu-24.04`.
- §4.1 / §4.2 / §4.3 / §4.4: runners updated to `ubuntu-24.04`.
- §4.2: feature-flag name / `cargo` exclusion form is explicitly deferred to
  the workspace-layout spec / impl PR (open question §9.4).
- §4.3 step 2: removed contradictory "install build deps already in image"
  wording — image provides them, job verifies presence and fails fast on
  drift.
- §4.4: cross-toolchain image preferred over `docker buildx`/QEMU; QEMU
  fallback gets an explicit 20-min wall-time budget so it doesn't silently
  blow the §8 8-min target.
- §4.5: required check name pinned to `dco`; implementer must rename if the
  underlying tool exposes a different default.
- §5: `ghcr.io/r12f/infmon-vpp-dev` mirror MUST be set up from day one,
  independent of upstream maintenance status — CI reads from the mirror so
  Docker Hub outages don't hard-block PRs.
- §7: explicit callout that "Dismiss stale reviews on new commits" + required
  CODEOWNERS review = re-approval on every push.
- §9.4 (new): VPP feature-flag open question.
- §10: added measurable acceptance gate (all five required workflows green
  on the impl PR).
