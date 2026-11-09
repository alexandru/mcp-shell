"""Exercise the diagnostic's credential and output boundaries over real HTTP."""

import json
import os
from pathlib import Path
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from threading import Thread
import unittest


TOKEN = "synthetic-auth-smoke-test-token"
PRIVATE_OUTPUT = "private-command-output-must-stay-hidden"
SCRIPT = Path(__file__).resolve().parents[1] / "auth-smoke.py"


class DiagnosticTests(unittest.TestCase):
    def setUp(self) -> None:
        self.destinations: list[tuple[str, str | None]] = []
        destinations = self.destinations

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                if self.path == "/redirect":
                    self.send_response(307)
                    self.send_header("Location", "/destination")
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                destinations.append((self.path, self.headers.get("Authorization")))
                response = json.dumps({
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "result": {
                        "isError": False,
                        "structuredContent": {
                            "stdout": PRIVATE_OUTPUT,
                            "stderr": TOKEN,
                            "exit_code": 0,
                        },
                    },
                }).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(response)))
                self.end_headers()
                self.wfile.write(response)

            def log_message(self, *args: object) -> None:
                pass

        self.server = HTTPServer(("127.0.0.1", 0), Handler)
        self.worker = Thread(target=self.server.serve_forever, daemon=True)
        self.worker.start()

    def tearDown(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.worker.join(timeout=5)

    def run_diagnostic(self, path: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(SCRIPT),
             "--url", f"http://127.0.0.1:{self.server.server_port}{path}",
             "--gh-command", "gh read-only-example",
             "--az-command", "az read-only-example"],
            env={"PATH": os.environ.get("PATH", ""), "MCP_SHELL_TOKEN": TOKEN},
            capture_output=True, text=True, timeout=10, check=False,
        )

    def assert_private_output_hidden(self, result: subprocess.CompletedProcess[str]) -> None:
        output = result.stdout + result.stderr
        self.assertNotIn(TOKEN, output)
        self.assertNotIn(PRIVATE_OUTPUT, output)
        self.assertNotIn("read-only-example", output)

    def test_success_hides_command_output_and_credentials(self) -> None:
        result = self.run_diagnostic("/mcp")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(self.destinations, [("/mcp", f"Bearer {TOKEN}")] * 2)
        self.assert_private_output_hidden(result)

    def test_redirect_never_forwards_credentials_to_destination(self) -> None:
        result = self.run_diagnostic("/redirect")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(self.destinations, [])
        self.assert_private_output_hidden(result)


if __name__ == "__main__":
    unittest.main()
