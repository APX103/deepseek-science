#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = [
#   "a2a-sdk[http-server]==1.1.2",
#   "uvicorn==0.49.0",
# ]
# ///
"""Loopback-only, LLM-backed A2A v1 long-task sidecar for manual/E2E tests.

The sidecar deliberately does not read Deepseek Science settings or credentials. It invokes
one executable supplied with ``--llm-helper`` (plus repeatable, non-shell
``--llm-helper-arg`` values). The helper receives a UTF-8 prompt on stdin and must emit one JSON
object on stdout::

    {"text": "# Markdown result", "model": "model-id", "usage": {"total_tokens": 42}}

``content`` is accepted as a compatibility alias for ``text``. An optional integer
``llm_call_count`` may report helper-internal retries/calls. No other helper fields are exposed
through A2A. In particular, reasoning/thinking and credentials are never copied into the task.

Run with ``uv run --offline`` after dependencies have been cached when an isolated, no-package-
network E2E is required. The LLM helper itself controls whether it contacts a model provider.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import hashlib
import json
import math
import os
import random
import re
import secrets
import signal
import socket

from collections.abc import Mapping
from contextlib import asynccontextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import uvicorn

from google.protobuf.json_format import ParseDict
from google.protobuf.struct_pb2 import Value
from starlette.applications import Starlette

from a2a.server.agent_execution.agent_executor import AgentExecutor
from a2a.server.agent_execution.context import RequestContext
from a2a.server.events.event_queue import EventQueue
from a2a.server.request_handlers import DefaultRequestHandler
from a2a.server.routes import create_agent_card_routes, create_jsonrpc_routes
from a2a.server.tasks.inmemory_task_store import InMemoryTaskStore
from a2a.server.tasks.task_updater import TaskUpdater
from a2a.types import (
    AgentCapabilities,
    AgentCard,
    AgentInterface,
    AgentSkill,
    Part,
    Task,
    TaskState,
    TaskStatus,
)


LOOPBACK_HOST = "127.0.0.1"
MAX_USER_INPUT_BYTES = 64 * 1024
MAX_HELPER_PROMPT_BYTES = 96 * 1024
MAX_HELPER_STDOUT_BYTES = 2 * 1024 * 1024
MAX_HELPER_STDERR_BYTES = 64 * 1024
MAX_MARKDOWN_BYTES = 1024 * 1024
MAX_USAGE_NODES = 128
READ_CHUNK_BYTES = 64 * 1024
MODEL_ID_MAX_CHARS = 256
USAGE_KEY = re.compile(r"^[A-Za-z0-9_.-]{1,64}$")


class SafeTaskError(Exception):
    """Expected failure whose stable code is safe to return to the remote caller."""

    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


@dataclass(frozen=True)
class HelperResult:
    markdown: str
    model: str
    usage: dict[str, Any]
    llm_call_count: int


@dataclass(frozen=True)
class ServerOptions:
    helper: Path
    helper_args: tuple[str, ...]
    helper_timeout_seconds: float
    min_working_seconds: float


def _sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def _synthetic_sensitivity_data(nonce: str) -> list[dict[str, Any]]:
    """Create nonce-specific synthetic data to prove the response path is not canned."""

    rng = random.Random(int(nonce, 16))
    cases = [
        ("sodium_void_fraction", 5.0),
        ("fuel_temperature", 7.5),
        ("cladding_thermal_conductivity", -6.0),
        ("delayed_neutron_fraction", -4.0),
    ]
    rows: list[dict[str, Any]] = []
    for parameter, perturbation_pct in cases:
        rows.append(
            {
                "parameter": parameter,
                "perturbation_pct": perturbation_pct,
                "delta_peak_clad_temperature_c": round(rng.uniform(-8.0, 13.0), 3),
                "delta_reactivity_pcm": round(rng.uniform(-115.0, 145.0), 3),
            }
        )
    return rows


def _build_helper_prompt(
    user_task: str, nonce: str, sensitivity_data: list[dict[str, Any]]
) -> str:
    data_json = json.dumps(sensitivity_data, ensure_ascii=False, separators=(",", ":"))
    return f"""You are an independent nuclear-engineering research agent. Produce only the final
research response in Markdown; do not expose hidden reasoning, chain-of-thought, credentials,
or orchestration internals.

Address the user's task directly and distinguish evidence, assumptions, and proposed work.
Where relevant, focus on Generation-IV fast reactors and concrete opportunities for AI-assisted
research. The small table below is synthetic E2E challenge data, not experimental evidence and
must be labelled as synthetic. Briefly interpret it only as a workflow/sensitivity demonstration.
Include the run nonce exactly once so the caller can verify this was generated for this request.

RUN_NONCE: {nonce}
SYNTHETIC_FAST_REACTOR_SENSITIVITY_JSON: {data_json}

USER_TASK_BEGIN
{user_task}
USER_TASK_END
"""


def _sanitize_usage(value: Any) -> dict[str, Any]:
    nodes = 0

    def visit(item: Any, depth: int) -> Any:
        nonlocal nodes
        nodes += 1
        if nodes > MAX_USAGE_NODES or depth > 5:
            raise SafeTaskError("invalid_helper_usage")
        if item is None or isinstance(item, bool):
            return item
        if isinstance(item, int):
            return item
        if isinstance(item, float):
            if not math.isfinite(item):
                raise SafeTaskError("invalid_helper_usage")
            return item
        if isinstance(item, Mapping):
            result: dict[str, Any] = {}
            for key, child in item.items():
                if not isinstance(key, str) or not USAGE_KEY.fullmatch(key):
                    raise SafeTaskError("invalid_helper_usage")
                result[key] = visit(child, depth + 1)
            return result
        if isinstance(item, list):
            return [visit(child, depth + 1) for child in item]
        # Usage is intentionally numeric/structural only, so it cannot smuggle secrets or text.
        raise SafeTaskError("invalid_helper_usage")

    if not isinstance(value, Mapping):
        raise SafeTaskError("invalid_helper_usage")
    sanitized = visit(value, 0)
    assert isinstance(sanitized, dict)
    return sanitized


def _parse_helper_result(raw_stdout: bytes) -> HelperResult:
    try:
        decoded = raw_stdout.decode("utf-8")
        payload = json.loads(decoded)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise SafeTaskError("invalid_helper_json") from exc

    if not isinstance(payload, dict):
        raise SafeTaskError("invalid_helper_json")

    markdown = payload.get("text")
    if markdown is None:
        markdown = payload.get("content")
    if not isinstance(markdown, str) or not markdown.strip():
        raise SafeTaskError("missing_helper_text")
    if len(markdown.encode("utf-8")) > MAX_MARKDOWN_BYTES:
        raise SafeTaskError("helper_text_too_large")

    model = payload.get("model")
    if (
        not isinstance(model, str)
        or not model.strip()
        or len(model) > MODEL_ID_MAX_CHARS
        or any(ord(char) < 32 for char in model)
    ):
        raise SafeTaskError("invalid_helper_model")

    usage = _sanitize_usage(payload.get("usage"))
    llm_call_count = payload.get("llm_call_count", 1)
    if (
        isinstance(llm_call_count, bool)
        or not isinstance(llm_call_count, int)
        or not 1 <= llm_call_count <= 16
    ):
        raise SafeTaskError("invalid_llm_call_count")

    return HelperResult(
        markdown=markdown,
        model=model,
        usage=usage,
        llm_call_count=llm_call_count,
    )


async def _read_bounded(
    stream: asyncio.StreamReader | None, limit: int, error_code: str
) -> bytes:
    if stream is None:
        raise SafeTaskError("helper_pipe_unavailable")
    data = bytearray()
    while True:
        remaining = limit + 1 - len(data)
        chunk = await stream.read(min(READ_CHUNK_BYTES, remaining))
        if not chunk:
            break
        data.extend(chunk)
        if len(data) > limit:
            raise SafeTaskError(error_code)
    return bytes(data)


async def _stop_process(process: asyncio.subprocess.Process) -> None:
    if process.returncode is not None:
        return

    def send_process_signal(sig: signal.Signals) -> None:
        try:
            os.killpg(process.pid, sig)
        except OSError:
            # Sandboxed macOS runs can deny process-group signaling even for a direct child.
            # Fall back to the asyncio child-process handle without weakening the output limits.
            with contextlib.suppress(OSError):
                process.send_signal(sig)

    send_process_signal(signal.SIGTERM)
    try:
        await asyncio.wait_for(process.wait(), timeout=0.75)
        return
    except asyncio.TimeoutError:
        pass
    send_process_signal(signal.SIGKILL)
    with contextlib.suppress(asyncio.TimeoutError):
        await asyncio.wait_for(process.wait(), timeout=0.75)


async def _exchange_with_helper(
    process: asyncio.subprocess.Process, prompt_bytes: bytes
) -> tuple[int, bytes]:
    stdout_task = asyncio.create_task(
        _read_bounded(
            process.stdout, MAX_HELPER_STDOUT_BYTES, "helper_stdout_too_large"
        )
    )
    stderr_task = asyncio.create_task(
        _read_bounded(
            process.stderr, MAX_HELPER_STDERR_BYTES, "helper_stderr_too_large"
        )
    )
    try:
        if process.stdin is None:
            raise SafeTaskError("helper_pipe_unavailable")
        process.stdin.write(prompt_bytes)
        await process.stdin.drain()
        process.stdin.close()
        stdout, _stderr = await asyncio.gather(stdout_task, stderr_task)
        return_code = await process.wait()
        return return_code, stdout
    finally:
        for task in (stdout_task, stderr_task):
            if not task.done():
                task.cancel()
        await asyncio.gather(stdout_task, stderr_task, return_exceptions=True)


def _json_value(payload: dict[str, Any]) -> Value:
    value = Value()
    ParseDict(payload, value)
    return value


class RealLongTaskAgent(AgentExecutor):
    """A real long-task executor whose LLM boundary is a constrained subprocess."""

    def __init__(self, options: ServerOptions) -> None:
        self._options = options
        self._processes: dict[str, asyncio.subprocess.Process] = {}
        self._cancel_handoffs: dict[str, asyncio.Event] = {}
        self._process_lock = asyncio.Lock()

    async def execute(
        self, context: RequestContext, event_queue: EventQueue
    ) -> None:
        task_id = context.task_id
        context_id = context.context_id
        user_message = context.message
        if not task_id or not context_id or user_message is None:
            return

        cancel_handoff = asyncio.Event()
        async with self._process_lock:
            self._cancel_handoffs[task_id] = cancel_handoff

        try:
            await event_queue.enqueue_event(
                Task(
                    id=task_id,
                    context_id=context_id,
                    status=TaskStatus(state=TaskState.TASK_STATE_SUBMITTED),
                    history=[user_message],
                    metadata={
                        "implementation": "official-a2a-python-sdk",
                        "sdk_version": "1.1.2",
                        "execution": "llm-backed-long-task",
                    },
                )
            )

            updater = TaskUpdater(
                event_queue=event_queue,
                task_id=task_id,
                context_id=context_id,
            )
            working_message = updater.new_agent_message(
                parts=[
                    Part(
                        text="Running the delegated nuclear-engineering analysis.",
                        media_type="text/plain",
                    )
                ]
            )
            await updater.start_work(message=working_message)
        except asyncio.CancelledError:
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(cancel_handoff.wait(), timeout=1.0)
            async with self._process_lock:
                self._cancel_handoffs.pop(task_id, None)
            raise
        except BaseException:
            async with self._process_lock:
                self._cancel_handoffs.pop(task_id, None)
            raise
        working_started = asyncio.get_running_loop().time()

        try:
            user_task = context.get_user_input()
            if not user_task.strip():
                raise SafeTaskError("empty_user_task")
            if len(user_task.encode("utf-8")) > MAX_USER_INPUT_BYTES:
                raise SafeTaskError("user_task_too_large")

            nonce = secrets.token_hex(16)
            sensitivity_data = _synthetic_sensitivity_data(nonce)
            helper_prompt = _build_helper_prompt(user_task, nonce, sensitivity_data)
            prompt_bytes = helper_prompt.encode("utf-8")
            if len(prompt_bytes) > MAX_HELPER_PROMPT_BYTES:
                raise SafeTaskError("helper_prompt_too_large")

            process = await asyncio.create_subprocess_exec(
                os.fspath(self._options.helper),
                *self._options.helper_args,
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE,
                start_new_session=True,
            )
            async with self._process_lock:
                self._processes[task_id] = process

            try:
                return_code, raw_stdout = await asyncio.wait_for(
                    _exchange_with_helper(process, prompt_bytes),
                    timeout=self._options.helper_timeout_seconds,
                )
            except asyncio.TimeoutError as exc:
                await _stop_process(process)
                raise SafeTaskError("helper_timeout") from exc

            if return_code != 0:
                raise SafeTaskError("helper_failed")
            helper_result = _parse_helper_result(raw_stdout)

            elapsed = asyncio.get_running_loop().time() - working_started
            remaining = self._options.min_working_seconds - elapsed
            if remaining > 0:
                await asyncio.sleep(remaining)

            audit = {
                "schema": "dss.a2a.real-agent-audit.v1",
                "model": helper_result.model,
                "usage": helper_result.usage,
                "prompt_sha256": _sha256_text(helper_prompt),
                "response_sha256": _sha256_text(helper_result.markdown),
                "llm_call_count": helper_result.llm_call_count,
                "run_nonce": nonce,
                "synthetic_sensitivity_data": sensitivity_data,
            }
            await updater.add_artifact(
                parts=[
                    Part(
                        text=helper_result.markdown,
                        media_type="text/markdown",
                        filename="fast-reactor-research.md",
                    ),
                    Part(
                        data=_json_value(audit),
                        media_type="application/json",
                        filename="llm-audit.json",
                    ),
                ],
                name="llm-backed-fast-reactor-research",
                metadata={"synthetic_data": True, "audit_schema_version": 1},
                last_chunk=True,
            )
            completed_message = updater.new_agent_message(
                parts=[Part(text="Delegated analysis completed.", media_type="text/plain")]
            )
            await updater.complete(message=completed_message)
        except asyncio.CancelledError:
            # The SDK cancels execute() and invokes cancel() concurrently. Keep the producer's
            # queue alive until cancel() has enqueued the canonical terminal event; otherwise an
            # unlucky scheduling order can persist FAILED instead of CANCELED.
            with contextlib.suppress(asyncio.TimeoutError):
                await asyncio.wait_for(cancel_handoff.wait(), timeout=1.0)
            raise
        except SafeTaskError as exc:
            failure_message = updater.new_agent_message(
                parts=[
                    Part(
                        text=f"Delegated analysis failed ({exc.code}).",
                        media_type="text/plain",
                    )
                ]
            )
            await updater.failed(message=failure_message)
        except Exception:
            failure_message = updater.new_agent_message(
                parts=[
                    Part(
                        text="Delegated analysis failed (internal_error).",
                        media_type="text/plain",
                    )
                ]
            )
            await updater.failed(message=failure_message)
        finally:
            async with self._process_lock:
                process = self._processes.pop(task_id, None)
                self._cancel_handoffs.pop(task_id, None)
            if process is not None:
                await _stop_process(process)

    async def cancel(
        self, context: RequestContext, event_queue: EventQueue
    ) -> None:
        task_id = context.task_id or ""
        context_id = context.context_id or ""
        async with self._process_lock:
            cancel_handoff = self._cancel_handoffs.get(task_id)

        updater = TaskUpdater(
            event_queue=event_queue,
            task_id=task_id,
            context_id=context_id,
        )
        canceled_message = updater.new_agent_message(
            parts=[
                Part(
                    text="Delegated analysis was canceled and its helper process was stopped.",
                    media_type="text/plain",
                )
            ]
        )
        # Publish the terminal event before releasing execute() or waiting on process teardown.
        # The SDK cancels the producer coroutine immediately before calling this method.
        try:
            await updater.cancel(message=canceled_message)
        finally:
            if cancel_handoff is not None:
                cancel_handoff.set()


def _agent_card(endpoint: str) -> AgentCard:
    return AgentCard(
        name="Deepseek Science Real Fast-Reactor Research Agent",
        description=(
            "Loopback-only E2E specialist backed by one real LLM helper call. It returns standard "
            "A2A Task states and artifacts; it never exposes hidden reasoning or credentials."
        ),
        version="1.0.0-e2e",
        capabilities=AgentCapabilities(streaming=False, push_notifications=False),
        default_input_modes=["text/plain"],
        default_output_modes=["text/markdown", "application/json"],
        supported_interfaces=[
            AgentInterface(
                url=f"{endpoint}/a2a",
                protocol_binding="JSONRPC",
                protocol_version="1.0",
            )
        ],
        skills=[
            AgentSkill(
                id="fast-reactor-research",
                name="Fast-reactor research",
                description=(
                    "Runs an independent LLM-backed nuclear-engineering analysis as a cancellable "
                    "long-running A2A Task."
                ),
                tags=["nuclear-engineering", "generation-iv", "fast-reactor", "research"],
                examples=[
                    "Review recent Generation-IV fast-reactor progress and identify AI research opportunities."
                ],
                input_modes=["text/plain"],
                output_modes=["text/markdown", "application/json"],
            )
        ],
    )


def _build_app(endpoint: str, options: ServerOptions) -> Starlette:
    card = _agent_card(endpoint)
    request_handler = DefaultRequestHandler(
        agent_executor=RealLongTaskAgent(options),
        task_store=InMemoryTaskStore(),
        agent_card=card,
    )

    @asynccontextmanager
    async def lifespan(_app: Starlette):
        yield
        await request_handler.aclose()

    routes = [
        *create_agent_card_routes(agent_card=card),
        *create_jsonrpc_routes(request_handler=request_handler, rpc_url="/a2a"),
    ]
    return Starlette(routes=routes, lifespan=lifespan)


def _parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run the loopback-only official-SDK A2A v1 real-agent E2E sidecar."
    )
    parser.add_argument(
        "--llm-helper",
        required=True,
        help="Executable that reads a prompt from stdin and writes one JSON object to stdout.",
    )
    parser.add_argument(
        "--llm-helper-arg",
        action="append",
        default=[],
        help=(
            "One literal helper argument; repeat as needed. Arguments are passed directly "
            "without a shell (for example: --llm-helper-arg llm-once)."
        ),
    )
    parser.add_argument(
        "--port",
        type=int,
        default=41411,
        help="Loopback port; use 0 to select a free ephemeral port (default: 41411).",
    )
    parser.add_argument(
        "--helper-timeout-seconds",
        type=float,
        default=180.0,
        help="Hard timeout for the helper subprocess, 1-300 seconds (default: 180).",
    )
    parser.add_argument(
        "--min-working-seconds",
        type=float,
        default=1.25,
        help="Minimum observable WORKING duration, 1-10 seconds (default: 1.25).",
    )
    args = parser.parse_args()
    if not 0 <= args.port <= 65535:
        parser.error("--port must be between 0 and 65535")
    if not 1.0 <= args.helper_timeout_seconds <= 300.0:
        parser.error("--helper-timeout-seconds must be between 1 and 300")
    if not 1.0 <= args.min_working_seconds <= 10.0:
        parser.error("--min-working-seconds must be between 1 and 10")
    if len(args.llm_helper_arg) > 16 or any(
        "\0" in value or len(value.encode("utf-8")) > 1024
        for value in args.llm_helper_arg
    ):
        parser.error("--llm-helper-arg allows at most 16 values of at most 1024 bytes")
    return args


def _resolve_helper(raw_path: str) -> Path:
    helper = Path(raw_path).expanduser().resolve()
    if not helper.is_file() or not os.access(helper, os.X_OK):
        raise SystemExit("--llm-helper must name an existing executable file")
    return helper


def _bind_loopback(port: int) -> socket.socket:
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind((LOOPBACK_HOST, port))
    listener.listen(128)
    listener.setblocking(False)
    return listener


async def _serve(args: argparse.Namespace) -> None:
    helper = _resolve_helper(args.llm_helper)
    listener = _bind_loopback(args.port)
    actual_port = listener.getsockname()[1]
    endpoint = f"http://{LOOPBACK_HOST}:{actual_port}"
    options = ServerOptions(
        helper=helper,
        helper_args=tuple(args.llm_helper_arg),
        helper_timeout_seconds=args.helper_timeout_seconds,
        min_working_seconds=args.min_working_seconds,
    )
    app = _build_app(endpoint, options)
    config = uvicorn.Config(
        app,
        host=LOOPBACK_HOST,
        port=actual_port,
        log_level="warning",
        access_log=False,
    )
    server = uvicorn.Server(config)

    print(f"ENDPOINT={endpoint}", flush=True)
    print(f"AGENT_CARD={endpoint}/.well-known/agent-card.json", flush=True)
    print(f"A2A_JSONRPC={endpoint}/a2a", flush=True)
    print("READY", flush=True)
    try:
        await server.serve(sockets=[listener])
    finally:
        listener.close()


def main() -> None:
    args = _parse_args()
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(_serve(args))


if __name__ == "__main__":
    main()
