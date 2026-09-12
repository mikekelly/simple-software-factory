import http.client
import importlib.util
import json
import os
from pathlib import Path
import stat
import tempfile
import threading
import unittest
import unittest.mock


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("ssf_dashboard", ROOT / "dashboard.py")
dashboard = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(dashboard)


def row(issue_id, *, owner=None, active=True, subscriber_only=False, **values):
    repo, number = issue_id.rsplit("#", 1)
    result = {
        "id": issue_id,
        "repo": repo,
        "number": int(number),
        "title": f"Issue {number}",
        "url": f"https://github.com/{repo}/issues/{number}",
        "kind": "issue",
        "owner": owner if owner is not None else issue_id,
        "active": active,
        "subscriber_only": subscriber_only,
        "agent_state": "working",
    }
    result.update(values)
    return result


class GroupingTests(unittest.TestCase):
    def test_groups_owned_items_without_duplicate_cards(self):
        payload = {
            "sessions": [
                row("acme/widgets#1", last_assistant_message="Building it"),
                row("acme/widgets#2", owner="acme/widgets#1"),
                row("acme/widgets#3", owner="acme/widgets#1"),
                row("acme/widgets#4"),
            ]
        }
        cards = dashboard.dashboard_cards(payload)
        self.assertEqual([card["owner"] for card in cards], ["acme/widgets#1", "acme/widgets#4"])
        self.assertEqual([item["id"] for item in cards[0]["additional"]], ["acme/widgets#2", "acme/widgets#3"])
        self.assertEqual(cards[0]["last_assistant_message"], "Building it")

    def test_keeps_inactive_owner_for_an_active_child_and_omits_subscribers(self):
        payload = {
            "sessions": [
                row("acme/widgets#1", active=False, title="Original session"),
                row("acme/widgets#2", owner="acme/widgets#1", last_activity_at="2026-09-12T15:00:00Z"),
                row("acme/widgets#9", owner="", subscriber_only=True),
            ]
        }
        cards = dashboard.dashboard_cards(payload)
        self.assertEqual(len(cards), 1)
        self.assertEqual(cards[0]["origin"]["title"], "Original session")
        self.assertFalse(cards[0]["origin"]["active"])
        self.assertEqual(cards[0]["additional"][0]["id"], "acme/widgets#2")

    def test_rejects_javascript_issue_links(self):
        cards = dashboard.dashboard_cards({"sessions": [row("acme/widgets#1", url="javascript:alert(1)")]})
        self.assertIsNone(cards[0]["origin"]["url"])

    def test_surfaces_driver_failure_from_a_partial_snapshot(self):
        warning = dashboard.driver_warning(
            {"sessions": [], "orca": {"available": False, "error": "herdr: connection refused"}}
        )
        self.assertEqual(warning, "herdr: connection refused")


class SubprocessTests(unittest.TestCase):
    def fixture(self, source):
        temporary = tempfile.TemporaryDirectory()
        path = Path(temporary.name) / "ssf-fixture"
        path.write_text(source, encoding="utf-8")
        path.chmod(path.stat().st_mode | stat.S_IXUSR)
        return temporary, path

    def test_reads_fixture_status_without_live_ssf(self):
        temporary, path = self.fixture("#!/bin/sh\nprintf '%s' '{\"sessions\":[]}'\n")
        self.addCleanup(temporary.cleanup)
        self.assertEqual(dashboard.run_ssf_status((str(path),)), {"sessions": []})

    def test_reports_subprocess_failure(self):
        temporary, path = self.fixture("#!/bin/sh\necho 'daemon unavailable' >&2\nexit 7\n")
        self.addCleanup(temporary.cleanup)
        with self.assertRaisesRegex(dashboard.StatusError, "exit 7.*daemon unavailable"):
            dashboard.run_ssf_status((str(path),))

    def test_times_out_a_stuck_subprocess(self):
        temporary, path = self.fixture("#!/bin/sh\nsleep 1\n")
        self.addCleanup(temporary.cleanup)
        with self.assertRaisesRegex(dashboard.StatusError, "timed out"):
            dashboard.run_ssf_status((str(path),), timeout=0.02)


class RuntimeSecurityTests(unittest.TestCase):
    def test_rejects_a_state_url_that_is_not_exact_loopback_capability(self):
        token = "x" * 32
        self.assertIsNone(
            dashboard._state_url(
                {"token": token, "port": 80, "url": f"http://attacker.example/{token}/"},
                0,
            )
        )
        self.assertIsNone(
            dashboard._state_url(
                {"token": token, "port": 8123, "url": f"http://127.0.0.1:8123/{token}/extra"},
                0,
            )
        )

    def test_rejects_a_symlinked_fallback_state_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            (base / f"ssf-herdr-dashboard-{os.getuid()}").symlink_to(base)
            with unittest.mock.patch.dict(os.environ, {"XDG_RUNTIME_DIR": str(base)}, clear=False):
                with self.assertRaisesRegex(RuntimeError, "not a directory"):
                    dashboard._runtime_dir()


class HttpTests(unittest.TestCase):
    def setUp(self):
        source = dashboard.StatusSource(lambda: {"sessions": [row("acme/widgets#1")]})
        self.server = dashboard.DashboardServer(("127.0.0.1", 0), "test-token", source, 60)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def request(self, path, *, host=None, origin=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=2)
        headers = {"Host": host or f"127.0.0.1:{self.server.server_port}"}
        if origin is not None:
            headers["Origin"] = origin
        connection.request("GET", path, headers=headers)
        response = connection.getresponse()
        body = response.read()
        headers = dict(response.getheaders())
        connection.close()
        return response.status, headers, body

    def test_serves_tokenized_status_with_security_headers(self):
        status, headers, body = self.request("/test-token/api/status", origin=self.server.origin)
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body)["cards"][0]["owner"], "acme/widgets#1")
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertIn("default-src 'self'", headers["Content-Security-Policy"])

    def test_rejects_missing_token_foreign_host_and_foreign_origin(self):
        self.assertEqual(self.request("/api/status")[0], 404)
        self.assertEqual(self.request("/test-token/api/status", host="attacker.example")[0], 403)
        self.assertEqual(self.request("/test-token/api/status", origin="https://attacker.example")[0], 403)

    def test_turns_status_failure_into_safe_gateway_error(self):
        def fail():
            raise dashboard.StatusError("daemon unavailable")

        self.server.status_source = dashboard.StatusSource(fail)
        status, _, body = self.request("/test-token/api/status")
        self.assertEqual(status, 502)
        self.assertEqual(json.loads(body), {"error": "daemon unavailable"})


if __name__ == "__main__":
    unittest.main()
