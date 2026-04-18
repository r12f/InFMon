# Spec 008 — Packaging & Install (.deb, Ubuntu aarch64)

## Version history

| Version | Date       | Author      | Changes        |
| ------- | ---------- | ----------- | -------------- |
| 0.1     | 2026-04-18 | bf3 (agent) | Initial draft. |

| Field    | Value                                                                                  |
| -------- | -------------------------------------------------------------------------------------- |
| Tracking | DPU-14 (parent: DPU-4 — EPIC InFMon)                                                   |
| Repo     | https://github.com/r12f/InFMon                                                         |
| Depends  | spec 000 (overview), spec 004 (backend architecture)                                   |
| Affects  | spec 005 (frontend), spec 007 (CLI), spec 001 (CI — build job will produce the .deb)   |

## 1. Motivation

InFMon ships as four artefacts (a VPP plugin shared object, a Rust frontend
daemon, a Rust CLI, and a small set of config + systemd files). On the
target — a BlueField-3 DPU running Ubuntu Server 22.04 LTS for aarch64 —
operators expect to install software with `apt`, get a working systemd
service, and uninstall cleanly with `apt remove` / `apt purge`.

This spec defines the packaging contract so that:

- the build pipeline (spec 001) knows what to produce,
- the implementation PRs for backend / frontend / CLI know where their
  binaries land and what they may assume about the filesystem at runtime,
- and operators have a single, predictable install / upgrade / uninstall
  story that does not interfere with VPP installations they already manage.

## 2. Scope

In-scope:

- The set of `.deb` packages we ship, their names, and what each contains.
- File-system layout under `/usr`, `/etc`, `/var`.
- Dependency declarations, in particular the relationship to upstream VPP.
- systemd unit(s) for `infmon-frontend`, including ordering against
  `vpp.service`.
- Install / upgrade / uninstall (`remove` vs `purge`) flows and what state
  survives each.
- Versioning scheme (package version ↔ source version ↔ CHANGELOG entry).
- How the source tree is laid out so `dpkg-buildpackage` works
  out-of-the-box.

Out of scope:

- RPM / other distros (deferred; v2).
- x86_64 packages — InFMon's target is aarch64 BF-3. We will produce an
  x86_64 build for CI smoke only (spec 001), not a published .deb.
- Container images — separate spec if/when needed.
- Building / packaging VPP itself. We consume upstream packages from the
  fd.io repository (see §4).
- Signed-package / apt-repo hosting. The CI artefact is a raw `.deb`;
  hosting is an ops decision tracked outside this spec.

## 3. Packages

We ship **three** binary `.deb` packages, all built from a single source
package `infmon`:

| Package                  | Arch    | Contents                                                                 |
| ------------------------ | ------- | ------------------------------------------------------------------------ |
| `infmon-backend`         | `arm64` | The VPP plugin `infmon.so` + its API definition file.                    |
| `infmon-frontend`        | `arm64` | The `infmon-frontend` daemon binary, default config, systemd unit.       |
| `infmon-cli`             | `arm64` | The `infmonctl` CLI binary + shell completions + man page.               |

A meta-package `infmon` depends on all three so that `apt install infmon`
gives operators the full system in one step.

Splitting the three lets operators run the CLI from a jump host without
pulling VPP in, and lets us ship plugin-only updates without restarting
the frontend (the frontend tolerates plugin reload via the snapshot
contract from spec 004 §7).

## 4. Dependency on VPP

InFMon does **not** vendor VPP. The backend is a plugin loaded by the
operator's existing VPP installation.

### 4.1 Declared dependencies

`infmon-backend` declares:

```
Depends:  vpp (>= 24.10), vpp-plugin-core (>= 24.10), libc6, libstdc++6
Recommends: vpp-dpdk-dev-mlx (>= 24.10)
```

The minimum VPP version is pinned to whatever release the backend was
built against (spec 004 currently targets VPP 24.10). The version floor
will be bumped in lockstep with the build matrix in spec 001.

### 4.2 Install bootstrap

If VPP is not installed when the user runs `apt install infmon-backend`,
`apt` resolves the dependency from configured sources. We document the
fd.io apt-repo bootstrap in `debian/README.Debian` and reproduce the
canonical command in this spec for reference:

```bash
curl -fsSL https://packagecloud.io/fdio/release/gpgkey \
  | sudo gpg --dearmor -o /usr/share/keyrings/fdio-archive-keyring.gpg
echo "deb [signed-by=/usr/share/keyrings/fdio-archive-keyring.gpg] \
  https://packagecloud.io/fdio/release/ubuntu jammy main" \
  | sudo tee /etc/apt/sources.list.d/fdio.list
sudo apt update
sudo apt install infmon
```

We deliberately do **not** auto-add the fd.io repo from a maintainer
script: silently editing `/etc/apt/sources.list.d/` on install is hostile
and breaks the principle of least surprise. The dependency declaration
combined with documented bootstrap is sufficient.

### 4.3 Behaviour when VPP is already managed locally

Many BF-3 users build VPP from source (e.g. NVIDIA's BF reference image
ships its own VPP). In that case the operator passes
`--no-install-recommends` and uses `dpkg --force-depends` or installs an
equivalent locally-built VPP `.deb`. We document this path in
`debian/README.Debian` but do not weaken the declared dependency.

## 5. File-system layout

Authoritative install paths. Anything not listed here is forbidden — no
files outside `/usr`, `/etc`, `/var`, `/lib/systemd/system`, or
`/usr/share/doc/<package>`.

### 5.1 `infmon-backend`

| Path                                          | Owner | Mode | Notes                                                  |
| --------------------------------------------- | ----- | ---- | ------------------------------------------------------ |
| `/usr/lib/vpp_plugins/infmon.so`              | root  | 0644 | VPP picks this up automatically on next start.         |
| `/usr/share/vpp/api/plugins/infmon.api.json`  | root  | 0644 | Generated by `vppapigen`; consumed by Rust bindings.   |
| `/etc/infmon/backend.conf`                    | root  | 0644 | Conffile — `dpkg` preserves operator edits on upgrade. |
| `/usr/share/doc/infmon-backend/changelog.gz`  | root  | 0644 | Standard Debian changelog.                             |

The backend has no daemon of its own; its lifecycle is VPP's lifecycle.
On install / upgrade, the maintainer script does *not* restart VPP — it
prints a `NOTICE:` telling the operator to `systemctl restart vpp` when
they are ready. Restarting VPP from a package script would drop
production traffic on a DPU; we refuse to make that decision for the
operator.

### 5.2 `infmon-frontend`

| Path                                            | Owner          | Mode | Notes                                                     |
| ----------------------------------------------- | -------------- | ---- | --------------------------------------------------------- |
| `/usr/bin/infmon-frontend`                      | root           | 0755 | Daemon binary.                                            |
| `/etc/infmon/frontend.conf`                     | root           | 0644 | Conffile.                                                 |
| `/etc/infmon/flow_defs.d/`                      | root           | 0755 | Operator drops flow_def YAML here (consumed per spec 002).|
| `/lib/systemd/system/infmon-frontend.service`   | root           | 0644 | Unit file (§6).                                           |
| `/var/log/infmon/`                              | infmon:infmon  | 0750 | Created by postinst; rotated by `/etc/logrotate.d/infmon`.|
| `/etc/logrotate.d/infmon`                       | root           | 0644 | Daily rotation, 14 days, compressed.                      |
| `/var/lib/infmon/`                              | infmon:infmon  | 0750 | Reserved for future state (currently empty).              |
| `/usr/share/doc/infmon-frontend/changelog.gz`   | root           | 0644 |                                                           |

Postinst creates a system user `infmon` (group `infmon`) via `adduser
--system --group --no-create-home --home /var/lib/infmon infmon`. The
user is removed only on `purge`, not on `remove`, so an upgrade
round-trip does not orphan log files.

### 5.3 `infmon-cli`

| Path                                                | Owner | Mode | Notes                                  |
| --------------------------------------------------- | ----- | ---- | -------------------------------------- |
| `/usr/bin/infmonctl`                                | root  | 0755 | CLI binary (spec 007).                 |
| `/usr/share/bash-completion/completions/infmonctl`  | root  | 0644 | Generated at build time by clap.       |
| `/usr/share/zsh/vendor-completions/_infmonctl`      | root  | 0644 | Same source.                           |
| `/usr/share/man/man1/infmonctl.1.gz`                | root  | 0644 | Generated from clap by `clap_mangen`.  |
| `/usr/share/doc/infmon-cli/changelog.gz`            | root  | 0644 |                                        |

The CLI does not require VPP locally; it talks to the frontend's REST
surface (spec 005) over TCP or a Unix socket. `infmon-cli` therefore
declares no dependency on VPP.

## 6. systemd unit

`/lib/systemd/system/infmon-frontend.service`:

```ini
[Unit]
Description=InFMon flow-telemetry frontend
Documentation=https://github.com/r12f/InFMon
After=vpp.service network-online.target
Wants=network-online.target
# We cannot Require= vpp.service because some operators run a custom
# VPP unit name (e.g. vpp-bf3.service). BindsTo= would be wrong for the
# same reason. The frontend retries on the binary-API socket itself.
PartOf=vpp.service

[Service]
Type=notify
ExecStart=/usr/bin/infmon-frontend --config /etc/infmon/frontend.conf
Restart=on-failure
RestartSec=2s
User=infmon
Group=infmon
# The frontend reads the VPP stats segment (spec 004 §6) and talks to
# the binary-API Unix socket. Both live under /run/vpp by default.
SupplementaryGroups=vpp
RuntimeDirectory=infmon
RuntimeDirectoryMode=0750
StateDirectory=infmon
StateDirectoryMode=0750
LogsDirectory=infmon
LogsDirectoryMode=0750
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
NoNewPrivileges=true
# Stats segment is a hugepages-backed mmap; we need access but not write.
ReadOnlyPaths=/dev/hugepages
# Frontend opens its own listening socket (spec 005); if that ever
# requires CAP_NET_BIND_SERVICE for port < 1024 we will revisit.
CapabilityBoundingSet=
AmbientCapabilities=

[Install]
WantedBy=multi-user.target
```

Notes:

- `Type=notify`: the frontend calls `sd_notify(READY=1)` once it has
  successfully attached to the stats segment and the binary-API socket.
  This guarantees that `systemctl start infmon-frontend` blocks until
  the service is actually consuming counters, not merely forked.
- `PartOf=vpp.service` means a `systemctl restart vpp` automatically
  restarts the frontend (which is correct: a VPP restart invalidates
  the stats-segment mappings).
- The unit is installed enabled-by-default via `dh_installsystemd`'s
  default behaviour; operators who do not want it auto-started can
  `systemctl disable infmon-frontend` after install.

There is **no** unit for `infmon-backend` — the backend is a VPP plugin,
not a service. Its lifecycle is VPP's.

## 7. Install / upgrade / uninstall flows

### 7.1 Install

```
apt install infmon
  └─ resolves vpp >= 24.10 from fd.io repo (or local)
  └─ unpacks infmon-backend → /usr/lib/vpp_plugins/infmon.so
  └─ unpacks infmon-frontend → /usr/bin + /etc/infmon + unit file
  └─ unpacks infmon-cli → /usr/bin/infmonctl
  └─ postinst (frontend):
       creates infmon user/group (idempotent)
       creates /var/log/infmon, /var/lib/infmon
       deb-systemd-helper enable infmon-frontend.service
       deb-systemd-invoke start infmon-frontend.service
  └─ postinst (backend):
       prints NOTICE asking operator to restart VPP
```

The frontend will start cleanly even if VPP has not been restarted yet —
it simply parks in a retry loop on the binary-API socket and reports
`status=waiting_for_vpp` until the plugin appears. This is by design so
that the install order between the two packages does not matter.

### 7.2 Upgrade

- `infmon-backend` upgrade: replaces `/usr/lib/vpp_plugins/infmon.so`
  in place. VPP keeps the old plugin mapped until restarted; the postinst
  prints the same restart NOTICE. No service restart is performed.
- `infmon-frontend` upgrade: `dh_installsystemd` re-runs `daemon-reload`
  and restarts the unit. The snapshot/swap contract in spec 004 §7.2
  ensures no counters are lost across the restart window — at worst,
  one export interval is delayed.
- Conffiles (`/etc/infmon/*.conf`) follow standard Debian conffile
  handling: operator edits are preserved; on a packaged-default change
  `dpkg` prompts.

### 7.3 Uninstall

There are two levels, matching standard Debian semantics:

**`apt remove infmon-frontend infmon-backend infmon-cli`**

- Stops and disables `infmon-frontend.service`.
- Removes the plugin `.so` from `/usr/lib/vpp_plugins/` (so the next
  VPP restart will not load it).
- Removes binaries under `/usr/bin/` and the systemd unit file.
- **Leaves untouched:** `/etc/infmon/`, `/var/log/infmon/`,
  `/var/lib/infmon/`, the `infmon` system user, and **VPP itself**.

**`apt purge infmon-frontend infmon-backend infmon-cli`**

- Everything from `remove`, plus:
- Removes `/etc/infmon/` (including operator-edited conffiles).
- Removes `/var/log/infmon/` and `/var/lib/infmon/`.
- Removes the `infmon` system user and group.
- **Still leaves VPP alone.** We never call `apt remove vpp` from a
  maintainer script.

The "leave VPP alone" rule is non-negotiable: VPP on a DPU may be
carrying production traffic for unrelated services (e.g. an SDN
data-plane). Removing InFMon must never put that traffic at risk.

### 7.4 Maintainer-script summary

| Script        | Package           | Action                                                                                                                          |
| ------------- | ----------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `postinst`    | `infmon-backend`  | Print NOTICE about `systemctl restart vpp`. No file changes beyond what `dpkg` already did.                                     |
| `postinst`    | `infmon-frontend` | Create user, dirs; `deb-systemd-helper enable`; `deb-systemd-invoke start`.                                                     |
| `prerm`       | `infmon-frontend` | `deb-systemd-invoke stop infmon-frontend.service`.                                                                              |
| `postrm`      | `infmon-frontend` | On `purge`: remove `/etc/infmon`, `/var/log/infmon`, `/var/lib/infmon`, and the `infmon` user. On `remove`: nothing.            |
| `postrm`     | `infmon-backend`  | Print NOTICE that `infmon.so` is gone and operator should restart VPP if they want to unload it now.                            |

All scripts are idempotent and follow Debian Policy §6.

## 8. Source-tree layout for `dpkg-buildpackage`

```
debian/
├── changelog              # dch-managed; mirrors top-level CHANGELOG.md
├── control                # declares all 3 binary packages + meta package
├── copyright              # Apache-2.0, machine-readable format
├── rules                  # dh $@ --with systemd, --buildsystem=cmake+cargo
├── compat                 # 13
├── source/format          # 3.0 (quilt)
├── infmon-backend.install
├── infmon-backend.postinst
├── infmon-backend.postrm
├── infmon-frontend.install
├── infmon-frontend.service        # picked up by dh_installsystemd
├── infmon-frontend.postinst
├── infmon-frontend.prerm
├── infmon-frontend.postrm
├── infmon-frontend.logrotate      # picked up by dh_installlogrotate
├── infmon-cli.install
├── infmon-cli.manpages
├── infmon-cli.bash-completion
└── README.Debian                  # fd.io repo bootstrap, custom-VPP notes
```

The `rules` file invokes the existing top-level build:

- `cmake --build build/backend` → `infmon.so`, `infmon.api.json`
- `cargo build --release -p infmon-frontend` → daemon binary
- `cargo build --release -p infmon-cli` → `infmonctl`
- `cargo run -p infmon-cli -- --generate-completions` → completions
- `clap_mangen` → man page

Build dependencies in `debian/control`:

```
Build-Depends:
  debhelper-compat (= 13),
  cmake,
  pkg-config,
  vpp-dev (>= 24.10),
  libvppinfra-dev (>= 24.10),
  cargo (>= 1.75),
  rustc (>= 1.75),
  clang,
```

Spec 001's CI builds the source package with `dpkg-buildpackage -us -uc
-b -aarm64` on an aarch64 runner (or `sbuild` in a BF-3 chroot) and
publishes the resulting `.deb`s as workflow artefacts.

## 9. Versioning & CHANGELOG

### 9.1 Scheme

InFMon uses **SemVer 2.0.0** (`MAJOR.MINOR.PATCH`) on the source release.

- `MAJOR` bumps on incompatible changes to the binary-API messages
  (spec 004 §7.1), the stats-segment descriptor layout (spec 004 §6),
  the frontend REST contract (spec 005), or the CLI argument grammar
  (spec 007) that the help text marks as stable.
- `MINOR` bumps on additive changes (new `flow_def` operators, new
  binary-API messages, new exporter formats).
- `PATCH` bumps on bugfixes and packaging-only changes.

The Debian package version is the upstream version verbatim, with the
Debian revision suffix used only when we re-roll the *packaging* without
changing the upstream source:

```
infmon (1.4.2-1)        # first .deb of upstream 1.4.2
infmon (1.4.2-2)        # packaging-only fix, same source
infmon (1.4.3-1)        # next upstream release
```

Pre-releases use the `~` separator so `dpkg --compare-versions` orders
them before the final: `1.5.0~rc1-1` < `1.5.0-1`.

### 9.2 Single source of truth

The top-level `CHANGELOG.md` (Keep a Changelog format, already added in
the bootstrap PR) is the canonical changelog. `debian/changelog` is
generated from it by a small `tools/sync-debian-changelog.py` script
invoked from `debian/rules` at source-package build time, so the two
never drift. Each `infmon` release tag (`vX.Y.Z`) creates exactly one
`CHANGELOG.md` entry and exactly one `debian/changelog` entry.

The package metadata exposes the changelog at
`/usr/share/doc/<package>/changelog.gz`, which `apt changelog infmon`
reads. Each binary package's changelog is the same content (the
project ships as one source release).

### 9.3 Link from packages to CHANGELOG

`debian/control` sets:

```
Homepage: https://github.com/r12f/InFMon
Vcs-Browser: https://github.com/r12f/InFMon
Vcs-Git: https://github.com/r12f/InFMon.git
```

and `debian/copyright` references the upstream `CHANGELOG.md` via the
`Source:` field, so `apt show infmon` points operators at both the repo
and the human-readable changelog.

## 10. Open questions

1. **Hugepages provisioning.** The frontend needs read access to
   `/dev/hugepages`. Should the package ship a `sysctl.d` snippet
   reserving hugepages, or is that the operator's responsibility?
   Tentative answer: operator's responsibility — VPP installation
   already handles it on standard BF-3 images.
2. **Dual-VPP coexistence.** If an operator installs InFMon while
   already running a custom VPP build, the plugin path may differ from
   `/usr/lib/vpp_plugins/`. We may need a `dpkg-divert`-based override
   in a follow-up; out of scope for v1.
3. **Apt repo hosting.** Hosting an InFMon apt repo (vs publishing raw
   `.deb`s via GitHub Releases) is an ops decision. v1 ships raw `.deb`s
   via Releases.

## 11. Acceptance

This spec is accepted (per the project's spec-first process — see spec
000) when it is merged to `main` with @banidoru's sign-off. After
acceptance, the implementation PR for the `debian/` tree may begin and
must conform to §3–§9 or amend this spec first.
