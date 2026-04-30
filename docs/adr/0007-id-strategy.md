---
id: ADR-0007
title: Domain identifier strategy
status: accepted
date: 2026-04-29
deciders: Efe Taşkıran
---

## Context

Identifier formats accumulated organically across WP-W2-03 through WP-W2-06. The bug-fix bundle that landed alongside `tasks/report.md` widened the divergence: migration `0002_constraints.sql` turned `mailbox.id` into an `INTEGER PRIMARY KEY AUTOINCREMENT` (report.md §K7), so the project now ships **five** distinct id strategies in a single SQLite schema:

| Domain | Format | Source |
|---|---|---|
| `agents.id` | raw ULID (26-char Crockford base32) | `Ulid::new().to_string()` |
| `runs.id` | `r-{ULID}` | `commands::runs::runs_create` |
| `panes.id` | `p-{ULID}` | `sidecar::terminal::TerminalRegistry::spawn_pane` |
| `servers.id` | slug (`filesystem`, `github`, …) | `mcp/manifests/*.json` |
| `workflows.id` | slug (`daily-summary`) | `db::seed_demo_workflow` |
| `mailbox.id` | autoincrement integer | migration 0002 |

Without a stated strategy, the frontend has to embed five parsing rules and the `bindings.ts` surface mixes `string` and `number` ids. New domains have no precedent to follow.

## Decision

Three id classes, each chosen by a single property of the row it identifies:

1. **User-namespaced sortable ULIDs** for resources where the user creates instances at runtime and wants stable, sortable identifiers carrying creation-time information. Format: `{prefix}-{ULID}`.

   | Domain | Prefix |
   |---|---|
   | `runs` | `r-` |
   | `panes` | `p-` |
   | `agents` (forward-going) | `a-` |

   Existing rows without a prefix (legacy `agents.id` raw ULIDs) are accepted by readers; writers emit the prefixed form. No data migration — `agents.id` is treated by readers as opaque, and the prefix is purely a forward-going convention.

2. **Author-stable slugs** for catalog rows that are bundled with the binary and ship from disk. The slug doubles as the manifest filename (`mcp/manifests/{slug}.json`) and the wire id, so a developer writing the manifest controls the id directly.

   | Domain | Source of truth |
   |---|---|
   | `servers` | `src-tauri/src/mcp/manifests/*.json` `id` field |
   | `workflows` | hardcoded in `db::seed_demo_workflow` (Week 2) → fixture system (Week 3) |

3. **Autoincrement integers** for append-only logs where insertion order is the only stable property and no human ever types or shares the id. The id is opaque and exists solely so frontend list-renderers have a stable React key.

   | Domain | Reason |
   |---|---|
   | `mailbox` | append-only event stream; rows are not addressed by id externally |

## Rationale

Each class is justified by what the id has to do, not by aesthetics:

- ULIDs encode creation time in their lexicographic order; combined with the prefix they double as a debugging aid (`r-01J0…` vs `p-01J0…`) and resist collision under concurrent inserts.
- Slugs win for catalog rows because the *file* is the source of truth — picking ULIDs there forces every `seed_*` function to remember the id of every shipped row.
- Autoincrement integers are the cheapest stable monotonic sequence SQLite gives us. The bug-fix bundle (report.md §K7) rejected rowid because `DELETE` reuses rowid; `INTEGER PRIMARY KEY AUTOINCREMENT` does not.

## Consequences

Implementation rules:

- New domain rows MUST pick one class. Inventing a sixth strategy requires a follow-up ADR.
- Prefixed ULIDs are produced by a single helper, `models::ids::new_id(prefix)`, so the prefix-string is centralised. WP-W2-03's existing call-sites can migrate to it incrementally; no flag-day.
- `bindings.ts` will surface `id: string` for ULID and slug ids, `id: number` for autoincrement ids. Frontend hooks treat all three as opaque; ordering is server-side.
- Tests may continue to seed short literal ids (`"a1"`, `"w1"`, `"s3"`) — they exercise SQL paths, not id format. The format invariants live in the helpers and are tested there.

## Alternatives considered

**All ULIDs, no autoincrement.** Rejected: it forces `mailbox` to carry a 26-char string id whose only consumer is a React list — paying serialization cost on every event for no reader benefit.

**All autoincrement, no ULIDs.** Rejected: loses time-sorted property; SQLite would also have to issue an extra round-trip to discover the id of a fresh insert before the code can emit it in an event payload (currently the sidecar emits ULIDs it already holds).

**UUID v7 instead of ULID.** Rejected for now: equivalent properties, larger ecosystem in JS, but the project already depends on `ulid` and the bug-fix paket landed without changing it. Revisit if a need to interop with non-Rust UUID v7 producers appears.

## Revisit

If Week 3 introduces user-shareable run ids (deep-linking from a notification to a specific run), evaluate whether the `r-` prefix is enough or whether human-friendly slugs (`r-2026-04-29-daily-summary-1`) would be better. The prefix scheme accommodates the change — slugs are still strings.
