#!/usr/bin/env python3
"""Launch and serve the SSF session dashboard.

The herdr action starts this file as a launcher.  The launcher leaves a small
detached loopback server behind and exits, because herdr keeps action output
pipes and an action slot open for the lifetime of the action process.
"""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import stat
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Callable, Iterable
from urllib.parse import urlsplit
import webbrowser


PLUGIN_ROOT = Path(__file__).resolve().parent
DEFAULT_IDLE_TIMEOUT = 300.0
STATUS_TIMEOUT = 30.0
MAX_MESSAGE_CHARS = 4000


class StatusError(RuntimeError):
    """A safe, user-facing failure from loading SSF status."""


def _text(value: Any, fallback: str = "") -> str:
    return value if isinstance(value, str) else fallback


def _web_url(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    parsed = urlsplit(value)
    if parsed.scheme not in ("http", "https") or not parsed.netloc:
        return None
    return value


def _issue(row: dict[str, Any], fallback_id: str) -> dict[str, Any]:
    issue_id = _text(row.get("id"), fallback_id)
    return {
        "id": issue_id,
        "title": _text(row.get("title"), issue_id),
        "url": _web_url(row.get("url")),
        "kind": _text(row.get("kind"), "issue"),
        "active": row.get("active") is True,
    }


def dashboard_cards(payload: Any) -> list[dict[str, Any]]:
    """Collapse status rows into one card for each active owning session."""
    if not isinstance(payload, dict) or not isinstance(payload.get("sessions"), list):
        raise StatusError("ssf returned status data in an unexpected format")

    rows = [row for row in payload["sessions"] if isinstance(row, dict)]
    by_id = {_text(row.get("id")): row for row in rows if _text(row.get("id"))}
    active = [
        row
        for row in rows
        if row.get("active") is True
        and row.get("subscriber_only") is not True
        and _text(row.get("owner"))
    ]

    owner_ids: list[str] = []
    for row in active:
        owner = _text(row.get("owner"))
        if owner not in owner_ids:
            owner_ids.append(owner)

    cards: list[dict[str, Any]] = []
    for owner in owner_ids:
        owned = [row for row in active if _text(row.get("owner")) == owner]
        primary = by_id.get(owner, {})
        candidates = ([primary] if primary else []) + owned
        # Bound rows describe the same workspace. Prefer the row with the most
        # recent reported activity, while retaining the actual owner as origin.
        runtime = max(
            candidates,
            key=lambda row: _text(row.get("last_activity_at")),
        )
        message_row = next(
            (
                row
                for row in sorted(
                    candidates,
                    key=lambda candidate: _text(candidate.get("last_activity_at")),
                    reverse=True,
                )
                if _text(row.get("last_assistant_message")).strip()
            ),
            {},
        )
        message = _text(message_row.get("last_assistant_message")).strip()
        cards.append(
            {
                "owner": owner,
                "origin": _issue(primary, owner),
                "additional": [
                    _issue(row, _text(row.get("id"), owner))
                    for row in owned
                    if _text(row.get("id")) != owner
                ],
                "agent_state": _text(runtime.get("agent_state"), "unknown"),
                "last_activity_at": _text(runtime.get("last_activity_at")) or None,
                "last_assistant_message": message[:MAX_MESSAGE_CHARS] or None,
                "harness": _text(primary.get("harness") or runtime.get("harness")),
                "model": _text(primary.get("model") or runtime.get("model")) or None,
            }
        )
    return cards


def driver_warning(payload: dict[str, Any]) -> str | None:
    """Return SSF's driver error when its otherwise valid snapshot is partial."""
    if payload.get("factory_reachable") is False:
        vm = payload.get("host_vm")
        state = _text(vm.get("state")) if isinstance(vm, dict) else ""
        return "SSF could not reach the guest factory" + (f" (VM {state})" if state else "")
    status = payload.get("orca")
    if not isinstance(status, dict) or status.get("available") is not False:
        return None
    detail = _text(status.get("error")).strip()
    return detail or "SSF could not reach one or more session drivers"


def run_ssf_status(
    command: Iterable[str] = ("ssf", "status", "--json"),
    timeout: float = STATUS_TIMEOUT,
) -> dict[str, Any]:
    """Read status without a shell and with a firm execution deadline."""
    try:
        result = subprocess.run(
            tuple(command),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
            check=False,
        )
    except FileNotFoundError as exc:
        raise StatusError("ssf was not found on PATH") from exc
    except subprocess.TimeoutExpired as exc:
        raise StatusError(f"ssf status timed out after {timeout:g} seconds") from exc
    except OSError as exc:
        raise StatusError(f"could not run ssf: {exc}") from exc

    if result.returncode != 0:
        detail = result.stderr.strip().splitlines()
        suffix = f": {detail[-1][:300]}" if detail else ""
        raise StatusError(f"ssf status failed (exit {result.returncode}){suffix}")
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise StatusError("ssf status returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise StatusError("ssf returned status data in an unexpected format")
    return payload


class StatusSource:
    """Serialize status calls and briefly share their result between clients."""

    def __init__(self, loader: Callable[[], dict[str, Any]] = run_ssf_status) -> None:
        self.loader = loader
        self.lock = threading.Lock()
        self.cached_at = 0.0
        self.cached: dict[str, Any] | None = None

    def snapshot(self) -> dict[str, Any]:
        with self.lock:
            now = time.monotonic()
            if self.cached is not None and now - self.cached_at < 1.0:
                return self.cached
            payload = self.loader()
            self.cached = {
                "cards": dashboard_cards(payload),
                "warning": driver_warning(payload),
            }
            self.cached_at = now
            return self.cached


class DashboardServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(
        self,
        address: tuple[str, int],
        token: str,
        status_source: StatusSource | None = None,
        idle_timeout: float = DEFAULT_IDLE_TIMEOUT,
    ) -> None:
        self.token = token
        self.status_source = status_source or StatusSource()
        self.idle_timeout = idle_timeout
        self.last_request_at = time.monotonic()
        super().__init__(address, DashboardHandler)

    @property
    def origin(self) -> str:
        return f"http://127.0.0.1:{self.server_port}"

    def touch(self) -> None:
        self.last_request_at = time.monotonic()


class DashboardHandler(BaseHTTPRequestHandler):
    server: DashboardServer
    protocol_version = "HTTP/1.1"

    def log_message(self, _format: str, *_args: Any) -> None:
        pass

    def _host_is_safe(self) -> bool:
        return self.headers.get("Host", "") == f"127.0.0.1:{self.server.server_port}"

    def _origin_is_safe(self) -> bool:
        origin = self.headers.get("Origin")
        return origin is None or origin == self.server.origin

    def _headers(self, status: int, content_type: str, length: int) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(length))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("X-Frame-Options", "DENY")
        self.send_header(
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; "
            "connect-src 'self'; img-src 'self'; object-src 'none'; "
            "base-uri 'none'; frame-ancestors 'none'",
        )
        self.end_headers()

    def _send(self, status: int, body: bytes, content_type: str) -> None:
        self._headers(status, content_type, len(body))
        if self.command != "HEAD":
            self.wfile.write(body)

    def _json(self, status: int, value: Any) -> None:
        self._send(
            status,
            json.dumps(value, separators=(",", ":")).encode("utf-8"),
            "application/json; charset=utf-8",
        )

    def do_HEAD(self) -> None:
        self.do_GET()

    def do_GET(self) -> None:
        if not self._host_is_safe() or not self._origin_is_safe():
            self._json(403, {"error": "request origin rejected"})
            return

        path = urlsplit(self.path).path
        parts = path.split("/", 2)
        if len(parts) != 3 or not secrets.compare_digest(parts[1], self.server.token):
            self._json(404, {"error": "not found"})
            return

        self.server.touch()
        relative = parts[2]
        if relative in ("", "index.html"):
            self._asset("index.html", "text/html; charset=utf-8")
        elif relative == "dashboard.css":
            self._asset("dashboard.css", "text/css; charset=utf-8")
        elif relative == "dashboard.js":
            self._asset("dashboard.js", "text/javascript; charset=utf-8")
        elif relative == "health":
            self._json(200, {"ok": True})
        elif relative == "api/status":
            try:
                snapshot = self.server.status_source.snapshot()
                self._json(200, {**snapshot, "refreshed_at": time.time()})
            except StatusError as exc:
                self._json(502, {"error": str(exc)})
            except Exception:
                self._json(502, {"error": "could not load SSF status"})
        else:
            self._json(404, {"error": "not found"})

    def _asset(self, name: str, content_type: str) -> None:
        try:
            body = (PLUGIN_ROOT / "static" / name).read_bytes()
        except OSError:
            self._json(500, {"error": "dashboard asset is missing"})
            return
        self._send(200, body, content_type)


def _runtime_dir() -> Path:
    configured = os.environ.get("HERDR_PLUGIN_STATE_DIR")
    if configured:
        path = Path(configured)
    else:
        base = Path(os.environ.get("XDG_RUNTIME_DIR", "/tmp"))
        path = base / f"ssf-herdr-dashboard-{os.getuid()}"
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    details = path.lstat()
    if stat.S_ISLNK(details.st_mode) or not stat.S_ISDIR(details.st_mode):
        raise RuntimeError(f"dashboard state path is not a directory: {path}")
    if details.st_uid != os.getuid():
        raise RuntimeError(f"dashboard state directory is not owned by this user: {path}")
    path.chmod(0o700)
    return path


def _factory_key() -> str:
    """Separate reusable servers whose `ssf` commands read different state."""
    identity = [str(Path(__file__).resolve()), shutil.which("ssf") or "ssf"]
    identity.extend(
        os.environ.get(name, "")
        for name in (
            "HOME", "SSF_CONFIG_DIR", "SSF_STATE_DIR", "SSF_VM_GUEST", "SSF_VM_NAME",
            "XDG_CONFIG_HOME", "XDG_STATE_HOME", "HERDR_COMMAND", "ORCA_CLI_COMMAND",
            "HERDR_SOCKET_PATH", "HERDR_CONFIG_PATH", "CODEX_HOME", "CLAUDE_CONFIG_DIR",
        )
    )
    return hashlib.sha256("\0".join(identity).encode("utf-8")).hexdigest()[:16]


def _write_state(path: Path, state: dict[str, Any]) -> None:
    temporary = path.with_suffix(".tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        json.dump(state, stream)
    os.replace(temporary, path)


def _read_state(path: Path) -> dict[str, Any] | None:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return value if isinstance(value, dict) else None


def _server_alive(url: str) -> bool:
    try:
        parsed = urlsplit(url)
        connection = http.client.HTTPConnection("127.0.0.1", parsed.port, timeout=0.5)
        connection.request(
            "GET",
            parsed.path + "health",
            headers={"Host": f"127.0.0.1:{parsed.port}"},
        )
        response = connection.getresponse()
        response.read()
        connection.close()
        return response.status == 200
    except (OSError, ValueError, http.client.HTTPException):
        return False


def _state_url(state: dict[str, Any], requested_port: int) -> str | None:
    token = state.get("token")
    url = state.get("url")
    port = state.get("port")
    if (
        not isinstance(token, str)
        or re.fullmatch(r"[A-Za-z0-9_-]{32,128}", token) is None
        or not isinstance(url, str)
        or not isinstance(port, int)
        or isinstance(port, bool)
        or not 1 <= port <= 65535
    ):
        return None
    try:
        parsed = urlsplit(url)
        parsed_port = parsed.port
    except ValueError:
        return None
    if (
        parsed.scheme != "http"
        or parsed.hostname != "127.0.0.1"
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed_port != port
        or parsed.path != f"/{token}/"
        or (requested_port and parsed_port != requested_port)
    ):
        return None
    return url


def ensure_server(idle_timeout: float, port: int = 0) -> str:
    runtime_dir = _runtime_dir()
    factory_key = _factory_key()
    state_path = runtime_dir / f"dashboard-{factory_key}.json"
    lock_path = runtime_dir / f"dashboard-{factory_key}.lock"
    lock_descriptor = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o600)
    with os.fdopen(lock_descriptor, "r+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        state = _read_state(state_path)
        state_url = _state_url(state, port) if state else None
        if state_url and _server_alive(state_url):
            return state_url

        token = secrets.token_urlsafe(32)
        command = [
            sys.executable,
            str(Path(__file__).resolve()),
            "--serve",
            "--token",
            token,
            "--state-file",
            str(state_path),
            "--idle-timeout",
            str(idle_timeout),
            "--port",
            str(port),
        ]
        subprocess.Popen(
            command,
            cwd=PLUGIN_ROOT,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            close_fds=True,
            start_new_session=True,
        )
        deadline = time.monotonic() + 4.0
        while time.monotonic() < deadline:
            state = _read_state(state_path)
            state_url = _state_url(state, port) if state and state.get("token") == token else None
            if state_url and _server_alive(state_url):
                return state_url
            time.sleep(0.05)
    raise RuntimeError("the dashboard server did not start")


def serve(token: str, state_file: Path, idle_timeout: float, port: int = 0) -> None:
    server = DashboardServer(("127.0.0.1", port), token, idle_timeout=idle_timeout)
    url = f"{server.origin}/{token}/"
    _write_state(
        state_file,
        {"pid": os.getpid(), "token": token, "url": url, "port": server.server_port},
    )
    server.timeout = min(1.0, max(0.05, idle_timeout / 4))
    try:
        while time.monotonic() - server.last_request_at < idle_timeout:
            server.handle_request()
    finally:
        server.server_close()
        state = _read_state(state_file)
        if state and state.get("token") == token:
            with contextlib.suppress(OSError):
                state_file.unlink()


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Open the SSF agent dashboard")
    parser.add_argument("--no-browser", action="store_true", help="print the URL instead of opening it")
    parser.add_argument("--port", type=int, default=0, help="loopback port (default: choose a free port)")
    parser.add_argument("--idle-timeout", type=float, default=DEFAULT_IDLE_TIMEOUT, help=argparse.SUPPRESS)
    parser.add_argument("--serve", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--token", help=argparse.SUPPRESS)
    parser.add_argument("--state-file", type=Path, help=argparse.SUPPRESS)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.idle_timeout <= 0:
        raise SystemExit("--idle-timeout must be greater than zero")
    if not 0 <= args.port <= 65535:
        raise SystemExit("--port must be between 0 and 65535")
    if args.serve:
        if not args.token or args.state_file is None:
            raise SystemExit("internal server arguments are incomplete")
        serve(args.token, args.state_file, args.idle_timeout, args.port)
        return 0

    try:
        url = ensure_server(args.idle_timeout, args.port)
    except (OSError, RuntimeError) as exc:
        print(f"Could not start SSF dashboard: {exc}", file=sys.stderr)
        return 1
    if args.no_browser or not webbrowser.open(url):
        print(url, flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
