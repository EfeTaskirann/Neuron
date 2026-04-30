---
id: ADR-0008
title: Sidecar IPC framing — length-prefixed vs NDJSON
status: accepted
date: 2026-04-29
deciders: Efe Taşkıran
---

## Context

Two sidecar surfaces ship in Week 2:

1. **Agent runtime** (`src-tauri/src/sidecar/agent.rs` ↔ `src-tauri/sidecar/agent_runtime/`). In-house Rust↔Python bridge for the LangGraph supervisor. WP-W2-04 chose 4-byte big-endian length-prefixed UTF-8 JSON frames with a 16 MiB cap (`framing.rs` on both sides).
2. **MCP client** (`src-tauri/src/mcp/client.rs`). Connects to external MCP servers (Anthropic spec 2024-11-05). WP-W2-05 used NDJSON — one UTF-8 JSON object per line, `\n`-terminated — because the MCP spec mandates that wire format.

The two sidecar surfaces therefore use different framing protocols. AGENT_LOG W2-05 noted the difference in passing; no ADR captured the rule for choosing one over the other, so a third sidecar (a vector store agent, an observability collector, an MCP session pool) would have to rediscover the trade-off.

## Decision

The framing protocol is chosen by what stands on the other end of the pipe:

- **External, spec-compliant peer → NDJSON.** MCP servers, future LSP-style integrations, anything where the peer is implemented by a third party against a published wire format. We have no control over their codec; the spec wins.
- **In-house, both ends ours → 4-byte BE length-prefixed JSON.** Agent runtime, future internal supervisors. We control both ends and can pin the codec to whichever shape gives us the strongest invariants for our reader.

Length-prefixed is preferred for in-house surfaces because it makes the reader trivially correct: `read_exact(4)` → length → `read_exact(length)` → frame. There is no escaping pass, no risk that an embedded `\n` inside a UTF-8 string body breaks framing, and no need to scan for delimiters. NDJSON has none of these properties, but its plain-text shape is what every MCP server emits, so for that case we accept the looser contract.

## Rationale

Reader correctness under partial reads is the asymmetry. Tokio's `read_line` on a `BufReader` is documented as cancellation-unsafe — if the future is dropped (timeout fires, task is aborted) mid-line, the buffer's internal state is corrupted. report.md §Y20 flagged this as a Week-3 risk for any long-lived MCP session pool. Length-prefixed framing on a fresh `read_exact` is cancellation-safe in the relevant sense: cancelled reads leave the framer in a recoverable state because the next read either re-issues `read_exact(4)` from scratch or fails-stop on a desync that the supervisor detects.

For external peers we accept the looser contract because the alternative (proxying NDJSON↔length-prefixed in-process) would add a translation layer with its own bugs.

## Consequences

- `src-tauri/src/sidecar/framing.rs` doc-comment names this ADR and points at it for the rationale.
- `src-tauri/src/mcp/client.rs` doc-comment names this ADR for the NDJSON choice.
- New sidecars must pick one of the two and reference this ADR in their doc-comment. Inventing a third codec (msgpack, framed protobuf, raw bytes) requires a follow-up ADR.
- Protocol *version* discipline is orthogonal to framing choice; see ADR-0009 (forward) for handshake versioning.

## Alternatives considered

**Unify on NDJSON for both surfaces.** Rejected: cancellation safety is a meaningful win for the Python sidecar, where Week-3 may add long-lived heartbeats and we do not want to discover stream-corruption bugs under load.

**Unify on length-prefixed for both surfaces.** Rejected: would require us to wrap external MCP servers in a proxy that translates NDJSON↔length-prefixed in-process. That proxy is exactly the maintenance debt this ADR exists to avoid.

**msgpack instead of JSON inside the length-prefixed envelope.** Rejected for Week 2: JSON debug-ability is worth more than the 20–30 % bandwidth savings on a localhost pipe carrying low-rate spans.

## Revisit

If a third in-house sidecar appears that has very high frame rates (per-token streaming, audio), evaluate whether the JSON encoder is the bottleneck and whether a binary codec (msgpack, postcard) makes sense for that surface specifically. The decision to switch is per-sidecar; the framing ADR does not need to flip.
