"""Hardcoded "Daily summary" workflow.

WP-W2-04 §"Scope":

    Demo workflow "daily-summary" hardcoded in Python:
      Planner (LLM) → fetch_docs (tool) + search_web (tool) →
      Reasoner (LLM) → human approval node

The workflow is implemented as plain async functions that emit span
events through an injected `SpanEmitter`. This keeps the testing
surface narrow — the unit test passes a recording emitter and
assertions read off the captured span sequence — while still letting
the real entry point wire the same callable into a LangGraph
`StateGraph`.

We do **not** drive span emission via LangChain `ChatModel` callbacks
(per WP-W2-04 §"Sub-agent reminders"): the schema stays clean when
the workflow node is the explicit emitter.
"""

from __future__ import annotations

import time
import uuid
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable, Optional

from agent_runtime.secrets import NoApiKey, get_provider_key


DAILY_SUMMARY_ID = "daily-summary"


# --------------------------------------------------------------------- #
# Span emission (transport-agnostic)                                    #
# --------------------------------------------------------------------- #


@dataclass
class Span:
    """In-flight span record. Field names match the camelCase wire form
    of `src-tauri/src/models.rs::Span` so the JSON we emit lands
    directly in `runs_spans` after the Rust-side serde rename round
    trip.
    """

    id: str
    run_id: str
    name: str
    type: str  # 'llm' | 'tool' | 'logic' | 'human' | 'http'
    t0_ms: int
    parent_span_id: Optional[str] = None
    duration_ms: Optional[int] = None
    attrs: dict[str, Any] = field(default_factory=dict)
    prompt: Optional[str] = None
    response: Optional[str] = None
    is_running: bool = True

    def to_wire(self) -> dict[str, Any]:
        """Emit the camelCase shape consumed by Rust's `Span` deserializer."""
        import json
        return {
            "id": self.id,
            "runId": self.run_id,
            "parentSpanId": self.parent_span_id,
            "name": self.name,
            "type": self.type,
            "t0Ms": self.t0_ms,
            "durationMs": self.duration_ms,
            "attrsJson": json.dumps(self.attrs),
            "prompt": self.prompt,
            "response": self.response,
            "isRunning": self.is_running,
        }


# Callable that delivers a span event somewhere — the real entrypoint
# wires this to `framing.write_frame`; tests pass a list-appending
# closure.
SpanEmitter = Callable[[str, Span], Awaitable[None]]


# --------------------------------------------------------------------- #
# LLM client adapter                                                    #
# --------------------------------------------------------------------- #


# Tests inject a fake to avoid hitting the network. Production uses
# `langchain_anthropic.ChatAnthropic`. The adapter signature is plain
# `(prompt: str) -> str` so we don't pull a LangChain abstraction into
# the test seam.
LlmCallable = Callable[[str], Awaitable[str]]


async def _default_anthropic_llm(prompt: str) -> str:  # pragma: no cover
    """Production LLM caller. Excluded from coverage because the unit
    tests always pass a fake — exercising it would mean a real
    Anthropic round-trip which violates hermeticity.
    """
    api_key = get_provider_key("anthropic")
    # Lazy import — keeps `pytest -k framing` from loading LangChain.
    from langchain_anthropic import ChatAnthropic
    from langchain_core.messages import HumanMessage

    chat = ChatAnthropic(
        model="claude-3-5-sonnet-latest",
        api_key=api_key,
        max_tokens=512,
    )
    resp = await chat.ainvoke([HumanMessage(content=prompt)])
    if isinstance(resp.content, str):
        return resp.content
    # Some providers return a list of content blocks.
    return "\n".join(
        block.get("text", "") if isinstance(block, dict) else str(block)
        for block in resp.content
    )


# --------------------------------------------------------------------- #
# Mock tool implementations (WP-W2-05 wires the real MCP tools)         #
# --------------------------------------------------------------------- #


async def _mock_fetch_docs(query: str) -> str:
    """Stand-in for `fetch_docs` tool. Returns canned text per
    WP-W2-04 §"Scope": tool nodes use mock implementations until
    WP-W2-05.
    """
    return (
        f"Found 3 docs matching '{query}':\n"
        f" - intro.md (last edited 2 days ago)\n"
        f" - architecture.md\n"
        f" - changelog.md"
    )


async def _mock_search_web(query: str) -> str:
    """Stand-in for `search_web` tool. Same caveat as `_mock_fetch_docs`."""
    return (
        f"Top results for '{query}':\n"
        f" 1. Example article — example.com\n"
        f" 2. Reference docs — example.org\n"
        f" 3. Discussion thread — example.net"
    )


# --------------------------------------------------------------------- #
# Span helper — emits 'created' and 'closed' events around an awaitable #
# --------------------------------------------------------------------- #


def _now_ms() -> int:
    return int(time.time() * 1000)


def _new_span_id() -> str:
    return f"s-{uuid.uuid4().hex[:12]}"


async def _emit_open(emitter: SpanEmitter, run_id: str, span: Span) -> None:
    """Send a `span.created` event with `is_running=True`."""
    span.is_running = True
    await emitter("span.created", span)


async def _emit_close(
    emitter: SpanEmitter,
    run_id: str,
    span: Span,
    *,
    response: Optional[str] = None,
    error: Optional[str] = None,
) -> None:
    """Send a `span.closed` event with `is_running=False` and the
    span's final duration, response, and (optionally) error in
    `attrs.error`.
    """
    span.duration_ms = _now_ms() - span.t0_ms
    span.is_running = False
    if response is not None:
        span.response = response
    if error is not None:
        span.attrs = dict(span.attrs)
        span.attrs["error"] = error
    await emitter("span.closed", span)


# --------------------------------------------------------------------- #
# Workflow entry point                                                  #
# --------------------------------------------------------------------- #


async def run_daily_summary(
    *,
    run_id: str,
    emitter: SpanEmitter,
    llm: Optional[LlmCallable] = None,
    fetch_docs: Optional[Callable[[str], Awaitable[str]]] = None,
    search_web: Optional[Callable[[str], Awaitable[str]]] = None,
) -> str:
    """Execute the daily-summary topology and return the final run status.

    The function returns `'success'` or `'error'`. Span events are
    delivered via `emitter`; the caller is responsible for writing the
    `run.completed` envelope.

    Optional injection points (`llm`, `fetch_docs`, `search_web`) are
    the seams that tests use to keep the run hermetic. The defaults
    point at the real Anthropic adapter and mock tool stubs.
    """
    llm = llm or _default_anthropic_llm
    fetch_docs = fetch_docs or _mock_fetch_docs
    search_web = search_web or _mock_search_web

    # 1. Planner (LLM) ---------------------------------------------------
    planner = Span(
        id=_new_span_id(),
        run_id=run_id,
        name="planner",
        type="llm",
        t0_ms=_now_ms(),
        attrs={"model": "anthropic", "node": "planner"},
        prompt="Plan a daily summary based on recent docs and web context.",
    )
    await _emit_open(emitter, run_id, planner)
    try:
        plan = await llm(planner.prompt or "")
    except NoApiKey as missing:
        # Charter §"Hard constraints" #2 — keys live in keychain. If
        # missing, surface a structured error span and end the run.
        await _emit_close(
            emitter,
            run_id,
            planner,
            error="no_api_key",
            response=f"missing key for provider '{missing.provider}'",
        )
        return "error"
    except Exception as exc:  # pragma: no cover - defensive
        await _emit_close(emitter, run_id, planner, error=str(exc))
        return "error"
    await _emit_close(emitter, run_id, planner, response=plan)

    # 2. Tool fan-out: fetch_docs + search_web ---------------------------
    fetch_span = Span(
        id=_new_span_id(),
        run_id=run_id,
        parent_span_id=planner.id,
        name="fetch_docs",
        type="tool",
        t0_ms=_now_ms(),
        attrs={"tool": "fetch_docs", "query": "daily"},
    )
    await _emit_open(emitter, run_id, fetch_span)
    try:
        docs = await fetch_docs("daily")
    except Exception as exc:  # pragma: no cover - defensive
        await _emit_close(emitter, run_id, fetch_span, error=str(exc))
        return "error"
    await _emit_close(emitter, run_id, fetch_span, response=docs)

    web_span = Span(
        id=_new_span_id(),
        run_id=run_id,
        parent_span_id=planner.id,
        name="search_web",
        type="tool",
        t0_ms=_now_ms(),
        attrs={"tool": "search_web", "query": "daily updates"},
    )
    await _emit_open(emitter, run_id, web_span)
    try:
        web = await search_web("daily updates")
    except Exception as exc:  # pragma: no cover - defensive
        await _emit_close(emitter, run_id, web_span, error=str(exc))
        return "error"
    await _emit_close(emitter, run_id, web_span, response=web)

    # 3. Reasoner (LLM) --------------------------------------------------
    reasoner = Span(
        id=_new_span_id(),
        run_id=run_id,
        parent_span_id=planner.id,
        name="reasoner",
        type="llm",
        t0_ms=_now_ms(),
        attrs={"model": "anthropic", "node": "reasoner"},
        prompt=(
            "Synthesize a brief daily summary using:\n"
            f"PLAN:\n{plan}\n\nDOCS:\n{docs}\n\nWEB:\n{web}"
        ),
    )
    await _emit_open(emitter, run_id, reasoner)
    try:
        summary = await llm(reasoner.prompt or "")
    except NoApiKey as missing:
        await _emit_close(
            emitter,
            run_id,
            reasoner,
            error="no_api_key",
            response=f"missing key for provider '{missing.provider}'",
        )
        return "error"
    except Exception as exc:  # pragma: no cover - defensive
        await _emit_close(emitter, run_id, reasoner, error=str(exc))
        return "error"
    await _emit_close(emitter, run_id, reasoner, response=summary)

    # 4. Human approval (no-op stub — Week 3 wires the real UI) ----------
    approval = Span(
        id=_new_span_id(),
        run_id=run_id,
        parent_span_id=reasoner.id,
        name="human_approval",
        type="human",
        t0_ms=_now_ms(),
        attrs={"node": "human_approval", "auto_approved": True},
    )
    await _emit_open(emitter, run_id, approval)
    await _emit_close(
        emitter,
        run_id,
        approval,
        response="auto-approved (Week 3 wires real approval UI)",
    )

    return "success"
