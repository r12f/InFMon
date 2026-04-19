# `ci/` — CI infrastructure

Implementation of the `lint` / `rust-test` / `cpp-test` / `cross-build` /
`dco` gates required by [`specs/001-ci-and-precommit.md`][spec].

## Layout

| Path                             | Purpose                                                                  |
| -------------------------------- | ------------------------------------------------------------------------ |
| `../.pre-commit-config.yaml`     | Pre-commit orchestrator (rustfmt, clippy, clang-format, cppcheck, …).    |
| `../.github/workflows/*.yml`     | GitHub Actions: `lint`, `rust-test`, `cpp-test`, `cross-build`, `dco`.   |
| `check-dco.sh`                   | `commit-msg` hook: rejects commits missing a `Signed-off-by:` trailer.   |
| `branch-protection.sh`           | Idempotent script that applies branch protection to `main` via `gh`.     |
| `apt-packages.txt`               | Canonical list of apt packages installed in CI jobs (reserved as a future `hashFiles()` cache-key input — not yet wired into any workflow). |

## Container image

`cpp-test` and the C/C++ leg of `cross-build` use **`ligato/vpp-base:24.10`**
(see [spec §5][spec]). The image is referenced by tag in workflows today; pin
by digest after the first successful run on `main`:

```bash
docker pull ligato/vpp-base:24.10
docker inspect --format='{{index .RepoDigests 0}}' ligato/vpp-base:24.10
# Replace the `image:` line in cpp-test.yml + cross-build.yml with the digest.
```

## Branch protection

`./branch-protection.sh [owner/repo]` configures `main` per
[spec §7][spec]: 1 approving review, code-owner review required, linear
history, and the five status checks (`lint`, `rust-test`, `cpp-test`,
`cross-build`, `dco`) gated.

It must be run by a repo admin once the workflows have produced at least one
green run — GitHub will not let you mark a check "required" until it has been
seen on a commit.

## Caching strategy

See [spec §8][spec] for the full table. Summary:

- `pre-commit` cache keyed on `.pre-commit-config.yaml`.
- `cargo` registry / git keyed on `Cargo.lock`; `target/` cached by
  `Swatinem/rust-cache@v2` (per-job, per-target).
- `ccache` keyed on `src/backend/**/CMakeLists.txt`, with separate
  arm64 namespace for the cross job.

## E2E is NOT in CI

E2E tests under `tests/` require the BlueField-3 bench machine and run
manually — see [spec §6][spec] and `Makefile`'s `e2e` target.

[spec]: ../specs/001-ci-and-precommit.md
