#!/usr/bin/env python3
"""Validate the installed macOS LaunchAgent and its local MCP endpoint."""

from __future__ import annotations

import hashlib
import hmac
import importlib.util
import os
from pathlib import Path
import plistlib
import re
import stat
import sys
import time
from types import ModuleType
from urllib.error import HTTPError, URLError
from urllib.request import OpenerDirector


AGENT_LABEL = "io.github.alexandru.mcp-shell"
REQUEST_TIMEOUT_SECONDS = 5
STARTUP_TIMEOUT_SECONDS = 60


class CheckFailure(Exception):
    """A validation phase failed without exposing private data."""


def require(condition: bool, phase: str) -> None:
    if not condition:
        raise CheckFailure(phase)


def is_private_file(path: Path) -> bool:
    try:
        mode = stat.S_IMODE(path.stat().st_mode)
    except OSError:
        return False
    return mode & 0o077 == 0


def load_auth_smoke_helpers() -> ModuleType:
    helper_path = Path(__file__).with_name("auth-smoke.py")
    try:
        spec = importlib.util.spec_from_file_location("mcp_shell_auth_smoke", helper_path)
        require(spec is not None and spec.loader is not None, "authentication helper")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    except CheckFailure:
        raise
    except Exception:
        raise CheckFailure("authentication helper") from None


def load_installation() -> tuple[str, str]:
    home = Path.home().resolve()
    config_dir = home / ".config" / "mcp-shell"
    binary = home / ".local" / "bin" / "mcp-shell"
    config_path = config_dir / "server.toml"
    init_path = config_dir / "init.bash"
    token_path = config_dir / "client-token"
    verifier_path = config_dir / "server-token.sha256"
    agent_path = home / "Library" / "LaunchAgents" / f"{AGENT_LABEL}.plist"

    try:
        import tomllib

        config = tomllib.loads(config_path.read_text(encoding="utf-8"))
        token = token_path.read_text(encoding="ascii")
        verifier = verifier_path.read_text(encoding="ascii").strip()
        agent_bytes = agent_path.read_bytes()
        agent = plistlib.loads(agent_bytes)
    except (OSError, UnicodeError, ValueError, plistlib.InvalidFileException):
        raise CheckFailure("installed files") from None

    require(binary.is_file() and os.access(binary, os.X_OK), "installed binary")
    require(config_dir.is_dir(), "configuration directory")
    require(stat.S_IMODE(config_dir.stat().st_mode) & 0o077 == 0, "private file permissions")
    for path in (config_path, init_path, token_path, verifier_path, agent_path):
        require(path.is_file() and is_private_file(path), "private file permissions")

    require(bool(re.fullmatch(r"[0-9a-f]{64}", token)), "client token")
    expected_verifier = hashlib.sha256(token.encode("ascii")).hexdigest()
    require(hmac.compare_digest(verifier, expected_verifier), "token verifier")

    expected_hosts = {
        "127.0.0.1:8090",
        "localhost:8090",
        "host.docker.internal:8090",
    }
    require(
        config.get("bind") == "0.0.0.0:8090"
        and set(config.get("allowed_hosts", [])) == expected_hosts
        and config.get("allowed_origins") == []
        and config.get("allow_rules") == ["gh *", "az *"]
        and config.get("description")
        == "Use this tool for GitHub and Azure commands with authentication from the host user."
        and config.get("initialization_file") == str(init_path)
        and config.get("working_directory") == str(home),
        "server configuration",
    )

    environment = agent.get("EnvironmentVariables")
    program_arguments = agent.get("ProgramArguments")
    require(
        agent.get("Label") == AGENT_LABEL
        and isinstance(environment, dict)
        and environment.get("HOME") == str(home)
        and environment.get("MCP_SHELL_TOKEN_SHA256") == expected_verifier
        and isinstance(program_arguments, list)
        and program_arguments == [str(binary), "--config", str(config_path)]
        and token.encode("ascii") not in agent_bytes,
        "LaunchAgent configuration",
    )
    return "http://127.0.0.1:8090/mcp", token


def post_request(
    helper: ModuleType,
    opener: OpenerDirector,
    url: str,
    token: str,
    request_id: str,
) -> tuple[int, bytes]:
    request = helper.make_request(url, token, "gh --version", request_id)
    try:
        with opener.open(request, timeout=REQUEST_TIMEOUT_SECONDS) as response:
            return response.status, response.read(helper.MAX_RESPONSE_BYTES + 1)
    except HTTPError as error:
        return error.code, error.read(helper.MAX_RESPONSE_BYTES + 1)


def wait_for_endpoint(helper: ModuleType, opener: OpenerDirector, url: str) -> None:
    deadline = time.monotonic() + STARTUP_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        try:
            status, _ = post_request(
                helper,
                opener,
                url,
                "invalid-installer-smoke-token",
                "installer-readiness-check",
            )
        except (OSError, URLError, TimeoutError):
            time.sleep(0.25)
            continue
        require(status == 401, "LaunchAgent HTTP readiness or authentication")
        return
    raise CheckFailure("LaunchAgent HTTP readiness")


def check_authorized_cli(
    helper: ModuleType,
    opener: OpenerDirector,
    url: str,
    token: str,
) -> None:
    try:
        status, response_bytes = post_request(
            helper,
            opener,
            url,
            token,
            "installer-cli-check",
        )
    except (OSError, URLError, TimeoutError):
        raise CheckFailure("authorized MCP request") from None
    require(status == 200, "authorized MCP request")
    if len(response_bytes) > helper.MAX_RESPONSE_BYTES:
        raise CheckFailure("authorized MCP response")
    try:
        response_text = response_bytes.decode("utf-8")
    except UnicodeDecodeError:
        raise CheckFailure("authorized MCP response") from None
    result = helper.matching_result(response_text, "installer-cli-check")
    structured = result.get("structuredContent") if isinstance(result, dict) else None
    require(
        helper.successful_result(result)
        and isinstance(structured, dict)
        and isinstance(structured.get("stdout"), str)
        and structured["stdout"].startswith("gh version "),
        "authorized CLI execution",
    )


def main() -> int:
    try:
        helper = load_auth_smoke_helpers()
        endpoint_url, token = load_installation()
        opener = helper.make_opener(helper.parse_endpoint(endpoint_url), None)
        wait_for_endpoint(helper, opener, endpoint_url)
        check_authorized_cli(helper, opener, endpoint_url, token)
    except CheckFailure as error:
        print(f"macOS installer validation failed: {error}.", file=sys.stderr)
        return 1
    except Exception:
        print("macOS installer validation failed: unexpected validation error.", file=sys.stderr)
        return 1

    print("macOS LaunchAgent install and CLI execution smoke test passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
