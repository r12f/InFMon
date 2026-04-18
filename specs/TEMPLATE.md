# NNN — Title

> Canonical spec skeleton. Copy this file to `specs/NNN-<slug>.md` for new
> specs. Inline copies of this template in other docs (e.g. `000-overview.md`)
> are for reading convenience only — **this file is authoritative**.

## Version history

| Version | Date       | Author       | Changes        |
| ------- | ---------- | ------------ | -------------- |
| 0.1     | YYYY-MM-DD | Riff (r12f)  | Initial draft. |

> **Version-history rules.**
> - **One PR = exactly one row.** When you push fixes addressing review
>   comments on the same PR, **amend the existing row's `Changes` cell** —
>   do **not** add a new row per review iteration.
> - `Author` is the **GitHub display name and handle** of the PR author,
>   in the form `Name (handle)` — e.g. `Riff (r12f)`. Co-authors,
>   reviewers, and bots do not appear here.
> - `Date` is the date the PR was first opened; it does not change on
>   review-comment fixes.

## Metadata (optional)

Include only the fields that apply. All cross-spec references must be
**markdown links** to the target spec file, not bare names.

- **Parent epic:** `DPU-NN` (Multica issue, if any)
- **Depends on:** [`NNN-<slug>`](NNN-<slug>.md), [`NNN-<slug>`](NNN-<slug>.md)
- **Related:** [`NNN-<slug>`](NNN-<slug>.md)
- **Affects:** `<component>`, `<component>` (e.g. `infmon-backend`, `infmon-cli`)

> **Forbidden metadata.** Do **not** add any of the following — they will
> be stripped on sight:
> - `Owner` / `Owners` (the spec repo's commit history is the owner record)
> - `Status` (a spec is "draft" while its PR is open and "accepted" once merged)
> - `Reviewer` / `Reviewers` (GitHub PR reviewers are the source of truth)
> - `Last updated` (the version-history table already records this)
> - `Tracking issue` (specs are not tracked by issues; the PR itself is the unit of work)

---

## Context

What problem are we solving and why now? What constraints apply
(hardware, performance, compatibility)?

## Goals & Non-goals

- Goal: …
- Goal: …
- Non-goal: …

## Design

Detailed approach. Diagrams welcome. Data structures, algorithms,
ownership, threading/locking model, failure modes.

## Interfaces

APIs, CLIs, file formats, wire formats, config knobs. Include
exact signatures or schemas.

## Test plan

- Unit tests (what, where, which framework)
- Integration / E2E (what scenarios, what packet captures)
- Performance criteria, if any

## Open questions

Numbered list of unresolved items, each with a proposed default
and the decision-maker.
