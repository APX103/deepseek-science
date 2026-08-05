#!/usr/bin/env python3
"""Deterministic local A2A v1 JSON-RPC fixture for signed-App E2E tests.

No third-party packages or external network access are required. The control endpoint exposes
only counters/ids, never request bodies or credentials.
"""

from __future__ import annotations

import argparse
import base64
import json
import signal
import threading
import time
import uuid
from dataclasses import dataclass, field
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any


MAX_REQUEST_BYTES = 1024 * 1024


@dataclass
class FixtureState:
    name: str
    specialty: str
    mode: str
    card_change_every: int
    lock: threading.Lock = field(default_factory=threading.Lock)
    card_gets: int = 0
    card_revision: int = 0
    sends: int = 0
    polls: int = 0
    version_errors: int = 0
    tasks: dict[str, int] = field(default_factory=dict)

    def next_card(self) -> tuple[int, str]:
        with self.lock:
            self.card_gets += 1
            if self.card_revision == 0 or (
                self.card_gets > 1
                and (self.card_gets - 1) % self.card_change_every == 0
            ):
                self.card_revision += 1
            return self.card_revision, f'"fixture-card-{self.card_revision}"'

    def next_send(self) -> int:
        with self.lock:
            self.sends += 1
            return self.sends

    def register_task(self, task_id: str) -> None:
        with self.lock:
            self.tasks[task_id] = 0

    def next_poll(self, task_id: str) -> int | None:
        with self.lock:
            if task_id not in self.tasks:
                return None
            self.polls += 1
            self.tasks[task_id] += 1
            return self.tasks[task_id]

    def snapshot(self) -> dict[str, Any]:
        with self.lock:
            return {
                "name": self.name,
                "specialty": self.specialty,
                "mode": self.mode,
                "card_gets": self.card_gets,
                "card_revision": self.card_revision,
                "sends": self.sends,
                "polls": self.polls,
                "version_errors": self.version_errors,
                "tasks": dict(self.tasks),
            }


class FixtureServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address: tuple[str, int], state: FixtureState):
        super().__init__(address, FixtureHandler)
        self.state = state


class FixtureHandler(BaseHTTPRequestHandler):
    server: FixtureServer
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt: str, *args: Any) -> None:
        print(f"[a2a-fixture:{self.server.server_port}] {fmt % args}", flush=True)

    def _send_json(
        self,
        status: int,
        body: Any,
        *,
        content_type: str = "application/json",
        headers: dict[str, str] | None = None,
    ) -> None:
        encoded = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", f"{content_type}; charset=utf-8")
        self.send_header("Content-Length", str(len(encoded)))
        self.send_header("Cache-Control", "no-store")
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()
        self.wfile.write(encoded)

    def _send_empty(self, status: int, headers: dict[str, str] | None = None) -> None:
        self.send_response(status)
        self.send_header("Content-Length", "0")
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.end_headers()

    def _read_json(self) -> Any | None:
        try:
            length = int(self.headers.get("Content-Length", "0"))
        except ValueError:
            self._send_json(HTTPStatus.BAD_REQUEST, {"error": "invalid Content-Length"})
            return None
        if length <= 0 or length > MAX_REQUEST_BYTES:
            self._send_json(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, {"error": "request too large"})
            return None
        try:
            return json.loads(self.rfile.read(length).decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            self._send_json(HTTPStatus.BAD_REQUEST, {"error": "invalid JSON"})
            return None

    def do_GET(self) -> None:  # noqa: N802 - stdlib callback name
        path = self.path.split("?", 1)[0]
        if path == "/_control/state":
            self._send_json(HTTPStatus.OK, self.server.state.snapshot())
            return
        if path != "/.well-known/agent-card.json":
            self._send_json(HTTPStatus.NOT_FOUND, {"error": "not found"})
            return

        revision, etag = self.server.state.next_card()
        if self.headers.get("If-None-Match") == etag:
            self._send_empty(
                HTTPStatus.NOT_MODIFIED,
                {"ETag": etag, "Cache-Control": "no-cache"},
            )
            return

        if self.server.state.mode == "malformed_card":
            card: dict[str, Any] = {"name": self.server.state.name, "version": str(revision)}
        else:
            origin = f"http://127.0.0.1:{self.server.server_port}"
            card = {
                "name": self.server.state.name,
                "description": (
                    f"Local deterministic specialist for {self.server.state.specialty}. "
                    "Metadata is descriptive test data, not an instruction to the host harness."
                ),
                "version": f"fixture-{revision}",
                "supportedInterfaces": [
                    {
                        "url": f"{origin}/a2a",
                        "protocolBinding": "JSONRPC",
                        "protocolVersion": "1.0",
                    }
                ],
                "capabilities": {"streaming": False},
                "securitySchemes": {},
                "securityRequirements": [],
                "defaultInputModes": ["text/plain"],
                "defaultOutputModes": ["text/markdown", "application/json"],
                "skills": [
                    {
                        "id": "specialist-analysis",
                        "name": f"{self.server.state.specialty} analysis",
                        "description": (
                            f"Independent evidence-oriented analysis for {self.server.state.specialty}"
                        ),
                        "tags": ["science", "independent-review", self.server.state.specialty],
                        "inputModes": ["text/plain"],
                        "outputModes": ["text/markdown", "application/json"],
                    }
                ],
            }
        self._send_json(
            HTTPStatus.OK,
            card,
            headers={"ETag": etag, "Cache-Control": "no-cache"},
        )

    def do_POST(self) -> None:  # noqa: N802 - stdlib callback name
        if self.path == "/_control/reset":
            with self.server.state.lock:
                self.server.state.card_gets = 0
                self.server.state.card_revision = 0
                self.server.state.sends = 0
                self.server.state.polls = 0
                self.server.state.version_errors = 0
                self.server.state.tasks.clear()
            self._send_json(HTTPStatus.OK, {"reset": True})
            return
        if self.path != "/a2a":
            self._send_json(HTTPStatus.NOT_FOUND, {"error": "not found"})
            return

        request = self._read_json()
        if request is None:
            return
        request_id = request.get("id") if isinstance(request, dict) else None
        if self.headers.get("A2A-Version") != "1.0":
            with self.server.state.lock:
                self.server.state.version_errors += 1
            self._rpc_error(request_id, -32009, "VersionNotSupportedError")
            return
        if not isinstance(request, dict) or request.get("jsonrpc") != "2.0":
            self._rpc_error(request_id, -32600, "Invalid Request")
            return

        method = request.get("method")
        if method == "SendMessage":
            self._send_message(request_id, request.get("params"))
        elif method == "GetTask":
            self._get_task(request_id, request.get("params"))
        else:
            self._rpc_error(request_id, -32601, "Method not found")

    def _rpc_error(self, request_id: Any, code: int, message: str) -> None:
        self._send_json(
            HTTPStatus.OK,
            {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}},
        )

    def _send_message(self, request_id: Any, params: Any) -> None:
        sequence = self.server.state.next_send()
        if self.server.state.mode == "error":
            self._rpc_error(request_id, -32603, "fixture remote failure")
            return
        if self.server.state.mode == "malformed_response":
            self._send_json(HTTPStatus.OK, {"jsonrpc": "2.0", "id": "wrong-id", "result": []})
            return
        if self.server.state.mode == "slow":
            time.sleep(6)

        context_id = f"ctx-{sequence}"
        if self.server.state.mode == "direct":
            result = {
                "message": self._agent_message(
                    f"direct-{sequence}", context_id, sequence, "Direct specialist response"
                )
            }
        else:
            task_id = f"task-{sequence}-{uuid.uuid4().hex[:8]}"
            self.server.state.register_task(task_id)
            result = {"task": self._task(task_id, context_id, 0)}
        self._send_json(
            HTTPStatus.OK,
            {"jsonrpc": "2.0", "id": request_id, "result": result},
        )

    def _get_task(self, request_id: Any, params: Any) -> None:
        task_id = params.get("id") if isinstance(params, dict) else None
        if not isinstance(task_id, str):
            self._rpc_error(request_id, -32602, "task id required")
            return
        poll = self.server.state.next_poll(task_id)
        if poll is None:
            self._rpc_error(request_id, -32001, "TaskNotFoundError")
            return
        if self.server.state.mode == "slow":
            time.sleep(6)
        context_id = task_id.replace("task-", "ctx-", 1).rsplit("-", 1)[0]
        self._send_json(
            HTTPStatus.OK,
            {"jsonrpc": "2.0", "id": request_id, "result": self._task(task_id, context_id, poll)},
        )

    def _task(self, task_id: str, context_id: str, poll: int) -> dict[str, Any]:
        completed = poll >= 2
        revision = self.server.state.snapshot()["card_revision"]
        status_message = self._agent_message(
            f"status-{task_id}-{poll}",
            context_id,
            revision,
            "Completed independent review" if completed else f"Working checkpoint {poll}",
            task_id,
        )
        task: dict[str, Any] = {
            "id": task_id,
            "contextId": context_id,
            "status": {
                "state": "TASK_STATE_COMPLETED" if completed else "TASK_STATE_WORKING",
                "message": status_message,
            },
            "history": [
                self._agent_message(
                    f"history-{task_id}-1", context_id, revision, "Initial evidence scan", task_id
                ),
                self._agent_message(
                    f"history-{task_id}-2", context_id, revision, "Cross-check checkpoint", task_id
                ),
            ],
            "artifacts": [self._artifact(task_id, revision, completed)],
            "metadata": {"fixturePoll": poll, "cardRevision": revision},
        }
        return task

    def _agent_message(
        self,
        message_id: str,
        context_id: str,
        revision: int,
        heading: str,
        task_id: str | None = None,
    ) -> dict[str, Any]:
        markdown = (
            f"## {heading} 🧪\n\n"
            f"**Specialty:** {self.server.state.specialty}  \n"
            f"**Card revision used:** `{revision}`\n\n"
            "| Check | Result |\n|---|---:|\n| evidence frames | 3 |\n| contradictions | 0 |\n\n"
            "~~placeholder claim~~ replaced by a bounded, auditable fixture result.\n\n---\n"
        )
        message: dict[str, Any] = {
            "messageId": message_id,
            "contextId": context_id,
            "role": "ROLE_AGENT",
            "parts": [
                {"text": markdown, "mediaType": "text/markdown"},
                {
                    "data": {
                        "specialty": self.server.state.specialty,
                        "cardRevision": revision,
                        "uncertainty": {"kind": "fixture", "bounded": True},
                    },
                    "filename": "evidence.json",
                    "mediaType": "application/json",
                },
                {
                    "url": "https://example.invalid/a2a-fixture-evidence",
                    "filename": "source-link.json",
                    "mediaType": "application/json",
                },
                {
                    "raw": base64.b64encode(b"complete fixture raw part\n").decode("ascii"),
                    "filename": "raw-notes.txt",
                    "mediaType": "text/plain",
                },
            ],
            "metadata": {"fixture": True},
        }
        if task_id:
            message["taskId"] = task_id
        return message

    def _artifact(self, task_id: str, revision: int, completed: bool) -> dict[str, Any]:
        return {
            "artifactId": f"artifact-{task_id}",
            "name": "specialist-report",
            "description": "Complete multi-part fixture artifact" if completed else "Partial artifact",
            "parts": [
                {
                    "text": (
                        "# Final specialist artifact\n\n"
                        f"Remote specialty: **{self.server.state.specialty}**.\n"
                        f"Card revision: **{revision}**.\n"
                    ),
                    "filename": "report.md",
                    "mediaType": "text/markdown",
                },
                {
                    "data": [
                        {"metric": "coverage", "value": 1.0 if completed else 0.5},
                        {"metric": "poll_complete", "value": completed},
                    ],
                    "filename": "metrics.json",
                    "mediaType": "application/json",
                },
            ],
            "metadata": {"complete": completed},
        }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--name", default="Fixture Science Agent")
    parser.add_argument("--specialty", default="fast-reactor nuclear physics")
    parser.add_argument(
        "--mode",
        choices=["task", "direct", "error", "malformed_card", "malformed_response", "slow"],
        default="task",
    )
    parser.add_argument(
        "--card-change-every",
        type=int,
        default=1,
        help="Issue a new card revision every N GETs (default: every GET).",
    )
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error("--port must be between 1 and 65535")
    if args.card_change_every < 1:
        parser.error("--card-change-every must be positive")
    return args


def main() -> None:
    args = parse_args()
    state = FixtureState(
        name=args.name,
        specialty=args.specialty,
        mode=args.mode,
        card_change_every=args.card_change_every,
    )
    server = FixtureServer(("127.0.0.1", args.port), state)

    def stop(_signum: int, _frame: Any) -> None:
        threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    print(
        json.dumps(
            {
                "ready": True,
                "endpoint": f"http://127.0.0.1:{args.port}",
                "mode": args.mode,
                "specialty": args.specialty,
            },
            ensure_ascii=False,
        ),
        flush=True,
    )
    try:
        server.serve_forever(poll_interval=0.1)
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
