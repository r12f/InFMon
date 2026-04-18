# InFMon

**InFMon** (Infrastructure / In-network Flow Monitor) is a high-performance
network flow monitoring system built around a VPP-based dataplane with a
Rust control plane, frontend UI, and CLI tooling.

This repository hosts the full stack:

- **infmon-backend** — control plane / dataplane glue (Rust + C/C++ for VPP plugins).
- **infmon-frontend** — operator-facing web UI.
- **infmon-cli** — command-line client for operators and automation.

## Status

🚧 Early scaffolding. APIs, schemas, and component boundaries are still
being defined under [`specs/`](./specs/). Expect breaking changes. Roadmap
and active work are tracked in [GitHub Issues](https://github.com/r12f/InFMon/issues)
(see the bootstrap issue [DPU-5](https://github.com/r12f/InFMon/issues)).

## Repository layout

```
.
├── specs/                  Design specs, RFCs, and protocol documents
├── src/
│   ├── infmon-backend/     Backend (control plane + dataplane integration)
│   ├── infmon-frontend/    Web UI
│   └── infmon-cli/         Command-line client
├── tests/                  Integration / end-to-end tests
├── CHANGELOG.md            Release notes (Keep a Changelog format)
├── CODEOWNERS              Required reviewers
└── LICENSE                 Apache-2.0
```

## Specs

Detailed design lives under [`specs/`](./specs/). Start there to understand
the architecture, data model, and component contracts before contributing
code.

## Building

Build instructions per component will land alongside each component's
initial implementation. See the README inside each `src/<component>/`
directory once available.

## Contributing

1. Open or pick up an issue.
2. Branch from `main`.
3. Submit a PR — all changes require review from a code owner
   (see [`CODEOWNERS`](./CODEOWNERS)) and must pass required CI checks.
4. Sign off your commits (`git commit -s`). The DCO sign-off is enforced
   by the `dco` CI check (added in PR #10 / DPU-15) so missing sign-offs
   block merge.

## License

Licensed under the [Apache License, Version 2.0](./LICENSE).
