#!/usr/bin/env python3
"""Validate the installed Ubuntu systemd user service and its MCP endpoint."""

from __future__ import annotations

import hashlib
import hmac
import importlib.util
import os
from pathlib import Path
import stat
import subprocess
import sys
import time
from types import ModuleType
from urllib.error import HTTPError, URLError
from urllib.request import OpenerDirector


SERVICE_NAME = "mcp-shell.service"
REQUEST_TIMEOUT_SECONDS = 5
STARTUP_TIMEOUT_SECONDS = 30


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


def load_installation() -> tuple[str, str, Path, Path, str]:
    home = Path.home().resolve()
    config_dir = home / ".config" / "mcp-shell"
    unit_dir = home / ".config" / "systemd" / "user"
    binary = home / ".local" / "bin" / "mcp-shell"
    config_path = config_dir / "server.toml"
    init_path = config_dir / "init.bash"
    token_path = config_dir / "client-token"
    verifier_path = config_dir / "server-token.sha256"
    unit_path = unit_dir / SERVICE_NAME

    try:
        import tomllib

        config = tomllib.loads(config_path.read_text(encoding="utf-8"))
        token = token_path.read_text(encoding="ascii")
        verifier = verifier_path.read_text(encoding="ascii").strip()
        unit_bytes = unit_path.read_bytes()
    except (OSError, UnicodeError, ValueError):
        raise CheckFailure("installed files") from None

    require(binary.is_file() and os.access(binary, os.X_OK), "installed binary")
    for directory in (config_dir, unit_dir):
        try:
            directory_mode = stat.S_IMODE(directory.stat().st_mode)
        except OSError:
            raise CheckFailure("private file permissions") from None
        require(directory.is_dir() and directory_mode & 0o077 == 0, "private file permissions")
    for path in (config_path, init_path, token_path, verifier_path):
        require(path.is_file() and is_private_file(path), "private file permissions")
    try:
        unit_mode = stat.S_IMODE(unit_path.stat().st_mode)
    except OSError:
        raise CheckFailure("systemd unit permissions") from None
    require(unit_path.is_file() and unit_mode == 0o600, "systemd unit permissions")

    require(len(token) == 64 and all(character in "0123456789abcdefABCDEF" for character in token), "client token")
    expected_verifier = hashlib.sha256(token.encode("ascii")).hexdigest()
    require(hmac.compare_digest(verifier, expected_verifier), "token verifier")
    require(token.encode("ascii") not in unit_bytes, "systemd unit token privacy")

    expected_hosts = {
        "127.0.0.1:8090",
        "localhost:8090",
        "host.docker.internal:8090",
        "host.docker.internal:8443",
    }
    require(
        config.get("bind") == "127.0.0.1:8090"
        and set(config.get("allowed_hosts", [])) == expected_hosts
        and config.get("allowed_origins") == []
        and config.get("allow_rules") == ["gh *", "az *"]
        and config.get("initialization_file") == str(init_path)
        and config.get("working_directory") == str(home),
        "server configuration",
    )
    return f"http://{config['bind']}/mcp", token, binary, config_path, expected_verifier


def run_checked(arguments: list[str], phase: str) -> str:
    try:
        result = subprocess.run(arguments, check=False, capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        raise CheckFailure(phase) from None
    require(result.returncode == 0, phase)
    return result.stdout.strip()


def service_pid() -> int:
    output = run_checked(
        ["systemctl", "--user", "show", "--property=ActiveState", "--property=MainPID", SERVICE_NAME],
        "systemd user service status",
    )
    properties = dict(line.split("=", 1) for line in output.splitlines() if "=" in line)
    require(properties.get("ActiveState") == "active", "systemd user service status")
    raw_pid = properties.get("MainPID", "")
    require(raw_pid.isdecimal() and int(raw_pid) > 0, "systemd user service process")
    return int(raw_pid)


def check_process(binary: Path, config_path: Path, expected_verifier: str) -> None:
    pid = service_pid()
    proc_dir = Path("/proc") / str(pid)
    try:
        status = (proc_dir / "status").read_text(encoding="ascii")
        command_line = (proc_dir / "cmdline").read_bytes().split(b"\0")
        environment = (proc_dir / "environ").read_bytes().split(b"\0")
    except OSError:
        raise CheckFailure("systemd user service process") from None

    uid_line = next((line for line in status.splitlines() if line.startswith("Uid:")), "")
    uid_fields = uid_line.split()
    require(len(uid_fields) >= 2 and uid_fields[1].isdecimal(), "systemd user service identity")
    require(int(uid_fields[1]) == os.getuid(), "systemd user service identity")
    expected_arguments = [os.fsencode(binary), b"--config", os.fsencode(config_path)]
    require(command_line[:3] == expected_arguments, "systemd user service command")

    process_environment = {
        entry.split(b"=", 1)[0]: entry.split(b"=", 1)[1]
        for entry in environment
        if b"=" in entry
    }
    expected_home = os.fsencode(str(Path.home().resolve()))
    expected_path = os.fsencode(os.environ.get("PATH", ""))
    require(
        process_environment.get(b"HOME") == expected_home
        and process_environment.get(b"PATH") == expected_path
        and process_environment.get(b"MCP_SHELL_TOKEN_SHA256") == expected_verifier.encode("ascii")
        and b"MCP_SHELL_TOKEN" not in process_environment,
        "systemd user service environment",
    )


def check_linger() -> None:
    raw_linger = run_checked(
        ["loginctl", "show-user", str(os.getuid()), "-p", "Linger", "--value"],
        "systemd user linger status",
    )
    require(raw_linger.lower() == "yes", "systemd user linger status")


def post_request(
    helper: ModuleType,
    opener: OpenerDirector,
    url: str,
    token: str,
    command: str,
    request_id: str,
) -> tuple[int, bytes]:
    request = helper.make_request(url, token, command, request_id)
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
                "gh --version",
                "installer-readiness-check",
            )
        except (OSError, URLError, TimeoutError):
            time.sleep(0.25)
            continue
        require(status == 401, "systemd service HTTP readiness or authentication")
        return
    raise CheckFailure("systemd service HTTP readiness")


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
            "gh --version",
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
        endpoint_url, token, binary, config_path, verifier = load_installation()
        check_linger()
        opener = helper.make_opener(helper.parse_endpoint(endpoint_url), None)
        wait_for_endpoint(helper, opener, endpoint_url)
        check_process(binary, config_path, verifier)
        check_authorized_cli(helper, opener, endpoint_url, token)
    except CheckFailure as error:
        print(f"Ubuntu installer validation failed: {error}.", file=sys.stderr)
        return 1
    except Exception:
        print("Ubuntu installer validation failed: unexpected validation error.", file=sys.stderr)
        return 1

    print("Ubuntu systemd user service install and CLI execution smoke test passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
