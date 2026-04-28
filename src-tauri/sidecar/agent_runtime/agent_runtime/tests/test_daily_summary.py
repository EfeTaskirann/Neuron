"""Daily-summary workflow tests.

WP-W2-04 §"Acceptance criteria" requires:

    Within ~5s, spans appear in `runs_spans` for the run
    (planner → tools → reasoner)

These tests exercise the workflow with a mocked LLM client, so the
hermetic test suite does not call any external API. The real Anthropic
adapter is excluded from coverage by the workflow module.
"""

from __future__ import annotations

import json
from typing import Any

import pytest

from agent_runtime.secrets import NoApiKey
from agent_runtime.workflows.daily_summary import (
    DAILY_SUMMARY_ID,
    Span,
    run_daily_summary,
)


class _SpanRecorder:
    """Captures every (event, span) pair emitted during a run."""

    def __init__(self) -> None:
        self.events: list[tuple[str, Span]] = []

    async def __call__(self, event: str, span: Span) -> None:
        # Snapshot the span's mutable fields at emit time so later
        # mutation (e.g., closing a span we previously opened) does
        # not retroactively rewrite the recorded value.
        self.events.append((
            event,
            Span(
                id=span.id,
                run_id=span.run_id,
                parent_span_id=span.parent_span_id,
                name=span.name,
                type=span.type,
                t0_ms=span.t0_ms,
                duration_ms=span.duration_ms,
                attrs=dict(span.attrs),
                prompt=span.prompt,
                response=span.response,
                is_running=span.is_running,
            ),
        ))


@pytest.mark.asyncio
async def test_happy_path_emits_planner_tools_reasoner_approval() -> None:
    """Workflow with a fake LLM produces the expected span sequence."""
    rec = _SpanRecorder()

    async def fake_llm(prompt: str) -> str:
        return f"OK: {prompt[:20]}..."

    status = await run_daily_summary(
        run_id="r-test",
        emitter=rec,
        llm=fake_llm,
    )

    assert status == "success"

    # 5 spans × 2 events (created + closed) = 10 events.
    assert len(rec.events) == 10, [e[0] for e in rec.events]

    names_in_order: list[str] = []
    for event, span in rec.events:
        if event == "span.created":
            names_in_order.append(span.name)

    # Acceptance: planner → fetch_docs → search_web → reasoner →
    # human_approval. The order is deterministic in the sequential
    # implementation; LangGraph's eventual real graph runner should
    # preserve it for this topology.
    assert names_in_order == [
        "planner",
        "fetch_docs",
        "search_web",
        "reasoner",
        "human_approval",
    ]


@pytest.mark.asyncio
async def test_happy_path_closes_every_span_with_duration() -> None:
    """Every `span.closed` carries `is_running=False` and a duration."""
    rec = _SpanRecorder()

    async def fake_llm(prompt: str) -> str:
        return "ok"

    await run_daily_summary(run_id="r-test", emitter=rec, llm=fake_llm)

    closed = [s for evt, s in rec.events if evt == "span.closed"]
    assert len(closed) == 5
    for span in closed:
        assert span.is_running is False
        assert span.duration_ms is not None and span.duration_ms >= 0


@pytest.mark.asyncio
async def test_no_api_key_path_emits_error_span_and_ends_in_error() -> None:
    """When the LLM raises `NoApiKey`, the planner closes with
    `attrs.error='no_api_key'` and the run terminates as `'error'`.
    """
    rec = _SpanRecorder()

    async def missing_key_llm(prompt: str) -> str:
        raise NoApiKey(provider="anthropic")

    status = await run_daily_summary(
        run_id="r-nokey",
        emitter=rec,
        llm=missing_key_llm,
    )

    assert status == "error"

    # Only the planner should have run, then closed with the structured
    # error attribute. Tools and reasoner are skipped.
    created = [s for evt, s in rec.events if evt == "span.created"]
    closed = [s for evt, s in rec.events if evt == "span.closed"]
    assert [s.name for s in created] == ["planner"]
    assert [s.name for s in closed] == ["planner"]
    assert closed[0].type == "llm"
    assert closed[0].attrs.get("error") == "no_api_key"
    assert closed[0].is_running is False


@pytest.mark.asyncio
async def test_wire_payload_uses_camelcase_keys() -> None:
    """`Span.to_wire()` emits the camelCase shape `bindings.ts` expects."""
    rec = _SpanRecorder()

    async def fake_llm(prompt: str) -> str:
        return "ok"

    await run_daily_summary(run_id="r-wire", emitter=rec, llm=fake_llm)

    first_span = rec.events[0][1]
    wire = first_span.to_wire()
    # Keys must match `src-tauri/src/models.rs::Span` field names
    # (camelCase via serde rename) verbatim.
    expected_keys = {
        "id",
        "runId",
        "parentSpanId",
        "name",
        "type",
        "t0Ms",
        "durationMs",
        "attrsJson",
        "prompt",
        "response",
        "isRunning",
    }
    assert set(wire.keys()) == expected_keys
    # `attrsJson` is a JSON string (not an object) — the Rust column
    # type is `TEXT` and we do not normalize on the sidecar side.
    assert isinstance(wire["attrsJson"], str)
    assert json.loads(wire["attrsJson"]) == first_span.attrs


@pytest.mark.asyncio
async def test_workflow_id_is_stable() -> None:
    """The dispatch id used over the wire is the canonical constant."""
    assert DAILY_SUMMARY_ID == "daily-summary"


@pytest.mark.asyncio
async def test_run_id_propagates_to_every_span() -> None:
    """Every emitted span carries the same `run_id`."""
    rec = _SpanRecorder()

    async def fake_llm(prompt: str) -> str:
        return "ok"

    await run_daily_summary(run_id="r-prop", emitter=rec, llm=fake_llm)

    for _, span in rec.events:
        assert span.run_id == "r-prop"
