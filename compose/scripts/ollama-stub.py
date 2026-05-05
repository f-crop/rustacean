#!/usr/bin/env python3
"""
Minimal Ollama stub for E2E testing.

Serves two endpoints that embed-worker needs:
  GET  /api/tags        — health check (returns empty model list)
  POST /api/embeddings  — returns zero-vector of the requested dimensions

Runs on port 11434 (Ollama default).
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DIMENSIONS = 768


class OllamaStubHandler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        # Log to stderr so embed-worker request activity is visible in compose logs.
        print(f"ollama-stub: {self.address_string()} {fmt % args}", file=sys.stderr, flush=True)

    def _send_json(self, status: int, body: dict) -> None:
        payload = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self) -> None:
        if self.path == "/api/tags":
            self._send_json(200, {"models": []})
        else:
            self._send_json(404, {"error": "not found"})

    def do_POST(self) -> None:
        if self.path == "/api/embeddings":
            length = int(self.headers.get("Content-Length", 0))
            self.rfile.read(length)  # consume body; we don't need it
            self._send_json(200, {"embedding": [0.0] * DIMENSIONS})
        else:
            self._send_json(404, {"error": "not found"})


if __name__ == "__main__":
    server = ThreadingHTTPServer(("0.0.0.0", 11434), OllamaStubHandler)
    print("ollama-stub: listening on :11434", flush=True)
    server.serve_forever()
