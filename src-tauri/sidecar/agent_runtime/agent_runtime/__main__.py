"""Sidecar entry point. JSON-RPC over length-prefixed stdio frames.

Spawned by `src-tauri/src/sidecar/agent.rs`. One running instance per
Neuron process. Holds no global state besides the framing pipe;
`run.start` requests fork an asyncio task per run so multiple runs can
overlap if the UI ever spawns them concurrently. Cancelling one run
**must not** kill the sidecar — the supervisor re-uses the process for
the lifetime of the app.

Inbound requests
----------------

```jsonc
{ "method": "run.start", "params": { "workflowId": "...", "runId": "r-..." } }
{ "method": "shutdown" }
```

Outbound events
---------------

```jsonc
{ "event": "span.created", "runId": "r-...", "span": { ... } }
{ "event": "span.updated", "runId": "r-...", "span": { ... } }
{ "event": "span.closed",  "runId": "r-...", "span": { ... } }
{ "event": "run.completed","runId": "r-...", "status": "success" | "error" }
{ "event": "ready" }                                  // emitted on startup
```
"""

from __future__ import annotations

import asyncio
import json
import sys
from typing import Any

from agent_runtime.framing import FrameError, read_frame, write_frame
from agent_runtime.workflows import WORKFLOWS
from agent_runtime.workflows.daily_summary import Span


def _stdout_buffer():
    """Return the binary stdout. Centralized so `pytest` capture
    monkey-patching has one place to redirect.
    """
    return sys.stdout.buffer


def _stdin_buffer():
    return sys.stdin.buffer


# --------------------------------------------------------------------- #
# Wire helpers                                                           #
# --------------------------------------------------------------------- #


_send_lock = asyncio.Lock()


async def _send_event(payload: dict[str, Any]) -> None:
    """Serialize and write one event frame to stdout under a lock so
    overlapping runs cannot interleave bytes.
    """
    body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
    async with _send_lock:
        # Switching to a thread for stdout is overkill on Windows where
        # stdout is non-blocking after `setmode(O_BINARY)`. We just
        # call the sync write — `flush()` returns once the OS pipe
        # accepts the bytes.
        write_frame(_stdout_buffer(), body)


def _make_emitter(run_id: str):
    async def emit(event: str, span: Span) -> None:
        await _send_event({
            "event": event,
            "runId": run_id,
            "span": span.to_wire(),
        })
    return emit


# --------------------------------------------------------------------- #
# Run dispatcher                                                         #
# --------------------------------------------------------------------- #


async def _start_run(workflow_id: str, run_id: str) -> None:
    """Resolve the workflow, run it, and emit `run.completed`.

    Errors inside the workflow surface as a `run.completed` with
    `status='error'` and an optional `error` field. We never let an
    exception escape this coroutine — the supervisor reads frames in a
    loop and an unhandled exception here would terminate the sidecar.
    """
    runner = WORKFLOWS.get(workflow_id)
    if runner is None:
        await _send_event({
            "event": "run.completed",
            "runId": run_id,
            "status": "error",
            "error": f"unknown workflow '{workflow_id}'",
        })
        return

    emitter = _make_emitter(run_id)
    try:
        status = await runner(run_id=run_id, emitter=emitter)
    except Exception as exc:  # pragma: no cover - defensive
        await _send_event({
            "event": "run.completed",
            "runId": run_id,
            "status": "error",
            "error": f"{type(exc).__name__}: {exc}",
        })
        return

    await _send_event({
        "event": "run.completed",
        "runId": run_id,
        "status": status,
    })


# --------------------------------------------------------------------- #
# Main loop                                                              #
# --------------------------------------------------------------------- #


async def _serve() -> int:
    """Read frames forever, dispatch them, return on EOF or shutdown."""
    await _send_event({"event": "ready"})

    loop = asyncio.get_running_loop()
    pending: set[asyncio.Task[Any]] = set()

    while True:
        # `read_frame` is blocking; offload to a worker thread so the
        # event loop can keep servicing run tasks.
        body = await loop.run_in_executor(None, read_frame, _stdin_buffer())
        if body == b"":
            break  # peer closed cleanly

        try:
            msg = json.loads(body.decode("utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError) as exc:
            await _send_event({"event": "error", "message": f"bad frame: {exc}"})
            continue

        method = msg.get("method")
        params = msg.get("params") or {}

        if method == "run.start":
            workflow_id = params.get("workflowId")
            run_id = params.get("runId")
            if not workflow_id or not run_id:
                await _send_event({
                    "event": "error",
                    "message": "run.start requires workflowId and runId",
                })
                continue
            task = asyncio.create_task(_start_run(workflow_id, run_id))
            pending.add(task)
            task.add_done_callback(pending.discard)
        elif method == "shutdown":
            break
        else:
            await _send_event({
                "event": "error",
                "message": f"unknown method '{method}'",
            })

    # Drain in-flight runs before exit so the run-completion events
    # actually reach the supervisor.
    if pending:
        await asyncio.gather(*pending, return_exceptions=True)
    return 0


def main() -> int:
    try:
        return asyncio.run(_serve())
    except (FrameError, KeyboardInterrupt):
        return 0


if __name__ == "__main__":
    sys.exit(main())
