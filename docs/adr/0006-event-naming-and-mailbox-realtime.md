---
id: ADR-0006
title: Tauri event naming convention and mailbox real-time delivery
status: accepted
date: 2026-04-28
deciders: Efe Taşkıran
---

## Context

Two adjacent issues surfaced during WP review:

1. Tauri event names are introduced ad-hoc across WPs: `run.{id}.span` (WP-W2-07), `pane.{id}.line` (WP-W2-06). Without a stated convention, every new event invites a "should it be `pane:line` or `pane.line` or `panes/line`?" round-trip during sub-agent execution.
2. WP-W2-08 originally specified `useMailbox` as a 2-second polling hook, while every other live data surface in the architecture (run spans, terminal lines) uses Tauri events. The mismatch is unmotivated: ADR-0005 explicitly frames events as the live-update mechanism, and `mailbox` is by definition cross-pane real-time signalling. Polling here is technical debt the day it ships.

## Decision

**Naming convention.** All Tauri events follow the shape `{domain}.{id?}.{verb}`, where:

- `{domain}` is the pluralized resource namespace as it appears in the command surface (`agents`, `runs`, `panes`, `mailbox`, `mcp`).
- `{id}` is included only when the event is scoped to a specific resource instance and consumers subscribe per-instance. Run spans and pane lines use `{id}`; mailbox-wide and registry-wide events do not.
- `{verb}` is a past-tense or noun event name (`created`, `updated`, `closed`, `span`, `line`, `new`, `installed`, `uninstalled`).

The full inventory for Week 2 is therefore:

```
runs.{id}.span             // span.created | span.updated | span.closed payloads
panes.{id}.line            // PTY output lines
mailbox.new                // new mailbox entry, payload = MailboxEntry
mcp.installed              // payload = Server (post-install)
mcp.uninstalled            // payload = { id }
agents.changed             // emitted on create/update/delete; payload = { id, op }
```

`agents.changed` is a single coalesced event for create/update/delete because the frontend invalidates the same query (`['agents']`) for all three. Splitting into three event names produces no caller benefit.

**Mailbox real-time delivery.** The `mailbox:emit` Tauri command, defined in WP-W2-03, performs the database insert and then emits a `mailbox.new` Tauri event whose payload is the inserted `MailboxEntry`. Frontend `useMailbox` subscribes to `mailbox.new`, merges new entries into the TanStack Query cache via `qc.setQueryData(['mailbox'], …)`, and unsubscribes on unmount. No polling.

## Rationale

The naming convention is small, predictable, and consistent with the command-surface convention (`agents:list`, `runs:get`). Sub-agents asked to add a new event have a single rule to follow rather than a precedent search.

The mailbox decision restores architectural consistency. ADR-0005 states the cache is the single source of truth and events are merged into the cache. A polling hook violates that by treating the cache as a render buffer for periodically-fetched snapshots. Polling also burns CPU and battery on a desktop app where the canonical event source (the local SQLite write) is already in-process — the cost of emitting one Tauri event per `mailbox:emit` call is negligible compared to a 2-second timer.

## Consequences

The WP-W2-03 specification for `mailbox:emit` gains one line: after the insert, emit `mailbox.new` with the inserted row as payload. Acceptance criteria gain one item: a unit test that asserts the event fires after a successful insert.

The WP-W2-08 hook list changes the `useMailbox` row from "polling every 2s" to "subscribes to `mailbox.new` events". The hook's implementation matches the `useRun` pattern in ADR-0005: initial fetch via `mailbox:list`, then a subscription that merges incoming entries into the query cache.

No schema changes. No new commands. No frontend mock shape changes.

## Alternatives considered

**Polling kept for simplicity.** Rejected: the simplicity argument fails when the rest of the app is event-driven, because consistency is itself a form of simplicity. New contributors learn one pattern instead of two.

**Server-Sent-Events-style envelope (`{event: "mailbox.new", data: …}`) on a single channel.** Rejected: Tauri's native event mechanism already provides per-event-name routing. Reinventing it adds a layer without value.

**Per-pane scoped events (`mailbox.{paneId}.new`).** Rejected: the mailbox is a cross-pane log by design. Frontend consumers are typically the mailbox view itself, which wants the global stream. Per-pane filtering, if ever needed, happens client-side.

## Revisit

If Week 3 introduces a high-frequency mailbox source (e.g., per-token streaming events repurposing the mailbox), revisit batching: emit `mailbox.new` at most every 50ms with an array payload instead of one event per row. Until then, one event per insert is correct.
