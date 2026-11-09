#!/usr/bin/env python3
"""Check read-only CLI authentication through a running MCP shell server."""

from __future__ import annotations

import argparse
import json
import os
import ssl
import sys
from typing import Any, Iterator
from urllib.parse import SplitResult, urlsplit
from urllib.request import (
    HTTPSHandler,
    HTTPRedirectHandler,
    OpenerDirector,
    ProxyHandler,
    Request,
    build_opener,
)


PROTOCOL_VERSION = "2026-07-28"
REQUEST_TIMEOUT_MS = 10_000
NETWORK_TIMEOUT_SECONDS = 30
MAX_RESPONSE_BYTES = 2 * 1024 * 1024
LOCAL_HTTP_HOSTS = frozenset(
    {"localhost", "127.0.0.1", "::1", "host.docker.internal"}
)


class EndpointConfigurationError(Exception):
    """The endpoint URL or TLS configuration is invalid."""


class RedirectRejected(Exception):
    """The MCP endpoint attempted an HTTP redirect."""


class RejectRedirectHandler(HTTPRedirectHandler):
    def redirect_request(
        self,
        request: Request,
        response: Any,
        code: int,
        message: str,
        headers: Any,
        new_url: str,
    ) -> None:
        raise RedirectRejected


def parse_endpoint(raw_url: str) -> SplitResult:
    if not raw_url or any(ord(character) <= 0x20 or ord(character) == 0x7F for character in raw_url):
        raise EndpointConfigurationError

    try:
        parsed = urlsplit(raw_url)
        hostname = parsed.hostname
        parsed.port
    except ValueError as error:
        raise EndpointConfigurationError from error

    if (
        parsed.scheme not in {"http", "https"}
        or not parsed.netloc
        or hostname is None
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or "?" in raw_url
        or "#" in raw_url
    ):
        raise EndpointConfigurationError

    if parsed.scheme == "http" and hostname.lower() not in LOCAL_HTTP_HOSTS:
        raise EndpointConfigurationError

    return parsed


def make_opener(parsed: SplitResult, ca_cert: str | None) -> OpenerDirector:
    try:
        tls_context = ssl.create_default_context(cafile=ca_cert)
    except (OSError, ssl.SSLError, ValueError) as error:
        raise EndpointConfigurationError from error

    proxy_handler = ProxyHandler({}) if parsed.scheme == "http" else ProxyHandler()
    handlers: list[Any] = [proxy_handler, RejectRedirectHandler()]
    if parsed.scheme == "https":
        handlers.append(HTTPSHandler(context=tls_context))
    return build_opener(*handlers)


def request_metadata() -> dict[str, Any]:
    return {
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": {
            "name": "mcp-shell-auth-smoke",
            "version": "0.1.0",
        },
        "io.modelcontextprotocol/clientCapabilities": {},
    }


def request_payload(command: str, request_id: str) -> bytes:
    payload = {
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "tools/call",
        "params": {
            "name": "execute",
            "arguments": {
                "command": command,
                "timeout_ms": REQUEST_TIMEOUT_MS,
            },
            "_meta": request_metadata(),
        },
    }
    return json.dumps(payload, separators=(",", ":")).encode("utf-8")


def make_request(url: str, token: str, command: str, request_id: str) -> Request:
    headers = {
        "Authorization": f"Bearer {token}",
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
        "MCP-Protocol-Version": PROTOCOL_VERSION,
        "Mcp-Method": "tools/call",
        "Mcp-Name": "execute",
    }
    return Request(
        url,
        data=request_payload(command, request_id),
        headers=headers,
        method="POST",
    )


def result_from_message(message: Any, request_id: str) -> dict[str, Any] | None:
    if not isinstance(message, dict) or message.get("id") != request_id:
        return None
    result = message.get("result")
    return result if isinstance(result, dict) else None


def sse_data_events(response_text: str) -> Iterator[str]:
    data_lines: list[str] = []
    for line in response_text.splitlines():
        if not line:
            if data_lines:
                yield "\n".join(data_lines)
                data_lines = []
            continue
        if line.startswith("data:"):
            value = line[5:]
            if value.startswith(" "):
                value = value[1:]
            data_lines.append(value)
    if data_lines:
        yield "\n".join(data_lines)


def matching_result(response_text: str, request_id: str) -> dict[str, Any] | None:
    try:
        message = json.loads(response_text)
    except json.JSONDecodeError:
        for event_data in sse_data_events(response_text):
            try:
                message = json.loads(event_data)
            except json.JSONDecodeError:
                return None
            result = result_from_message(message, request_id)
            if result is not None:
                return result
        return None
    return result_from_message(message, request_id)


def successful_result(result: dict[str, Any] | None) -> bool:
    if result is None or result.get("isError") is not False:
        return False
    structured = result.get("structuredContent")
    if not isinstance(structured, dict):
        return False
    exit_code = structured.get("exit_code")
    return type(exit_code) is int and exit_code == 0


def execute_check(
    opener: OpenerDirector,
    url: str,
    token: str,
    command: str,
    request_id: str,
) -> bool:
    try:
        request = make_request(url, token, command, request_id)
        with opener.open(request, timeout=NETWORK_TIMEOUT_SECONDS) as response:
            if response.status != 200:
                return False
            response_bytes = response.read(MAX_RESPONSE_BYTES + 1)
        if len(response_bytes) > MAX_RESPONSE_BYTES:
            return False
        response_text = response_bytes.decode("utf-8")
        return successful_result(matching_result(response_text, request_id))
    except Exception:
        return False


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Check GitHub and Azure CLI authentication through a running MCP shell server."
        ),
        epilog=(
            "The supplied commands are not inspected for side effects. The operator must "
            "choose read-only commands. A bearer token must be set in MCP_SHELL_TOKEN."
        ),
    )
    parser.add_argument("--url", required=True, help="MCP endpoint URL, including its path")
    parser.add_argument(
        "--gh-command",
        required=True,
        help="operator-reviewed read-only GitHub CLI command",
    )
    parser.add_argument(
        "--az-command",
        required=True,
        help="operator-reviewed read-only Azure CLI command",
    )
    parser.add_argument(
        "--ca-cert",
        help="optional CA certificate file for HTTPS server verification",
    )
    return parser.parse_args(argv)


def valid_bearer_token(token: str | None) -> bool:
    return bool(token) and all("!" <= character <= "~" for character in token)


def main(argv: list[str] | None = None) -> int:
    arguments = parse_args(argv)
    token = os.environ.get("MCP_SHELL_TOKEN")
    if token is None or not valid_bearer_token(token):
        print("Token validation failed: MCP_SHELL_TOKEN is missing or invalid.", file=sys.stderr)
        return 2

    try:
        parsed_url = parse_endpoint(arguments.url)
        opener = make_opener(parsed_url, arguments.ca_cert)
    except Exception:
        print("Endpoint validation failed.", file=sys.stderr)
        return 2

    endpoint_url = arguments.url
    checks = (
        ("GitHub CLI", arguments.gh_command, "auth-smoke-github"),
        ("Azure CLI", arguments.az_command, "auth-smoke-azure"),
    )
    all_checks_succeeded = True
    for phase, command, request_id in checks:
        if execute_check(opener, endpoint_url, token, command, request_id):
            print(f"{phase} authentication check passed.")
        else:
            print(f"{phase} authentication check failed.", file=sys.stderr)
            all_checks_succeeded = False
    return 0 if all_checks_succeeded else 1


if __name__ == "__main__":
    raise SystemExit(main())
