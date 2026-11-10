"""Exercise the Ubuntu installer with an isolated home and fake systemd tools."""

import hashlib
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "linux-install.sh"
UNIT = "mcp-shell.service"
GENERATED_TOKEN = "0" * 64


class UbuntuInstallerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory(prefix="mcp-linux-installer-test-")
        self.addCleanup(self.temporary_directory.cleanup)
        self.base = Path(self.temporary_directory.name)
        self.home = self.base / 'Home space \'single\' "double" & back\\slash $percent%'
        self.home.mkdir()
        self.fake_bin = self.base / "fake-bin"
        self.fake_bin.mkdir()
        self.log = self.base / "commands.log"
        self.linger_state = self.base / "linger-state"
        self.service_state = self.base / "service-state"
        self.clone_path = self.base / "clone-path"
        self.show_count = self.base / "show-count"
        self.real_openssl = shutil.which("openssl")
        if self.real_openssl is None:
            self.fail("openssl is required to exercise the installer digest path")
        odd_path = self.base / 'missing $dollar% "quote" \\slash path'
        self.env = {
            "PATH": f'{self.fake_bin}:{odd_path}:/usr/bin:/bin',
            "TMPDIR": str(self.base),
            "FAKE_LOG": str(self.log),
            "FAKE_LINGER_STATE": str(self.linger_state),
            "FAKE_SERVICE_STATE": str(self.service_state),
            "FAKE_CLONE_PATH_FILE": str(self.clone_path),
            "FAKE_SHOW_COUNT": str(self.show_count),
            "FAKE_UID": "1000",
            "FAKE_MANAGER_AVAILABLE": "1",
            "FAKE_LINGER_FAIL": "0",
            "FAKE_RESTART_FAIL": "0",
            "FAKE_PID_MODE": "stable",
            "FAKE_STARTUP_PID": "1301",
            "FAKE_BUILD_FAIL": "0",
            "FAKE_RUSTC_HOST": "x86_64-unknown-linux-gnu",
            "REAL_OPENSSL": self.real_openssl,
        }
        self.write_fake_tools()

    def write_fake_tools(self) -> None:
        self.write_tool("uname", '#!/bin/sh\nprintf "Linux\\n"\n')
        self.write_tool("id", '#!/bin/sh\nprintf "%s\\n" "$FAKE_UID"\n')
        self.write_tool(
            "git",
            r'''#!/bin/sh
printf 'git %s\n' "$*" >> "$FAKE_LOG"
if [ "$1" = "check-ref-format" ]; then
    exit 0
fi
if [ "$1" = "clone" ]; then
    destination=$4
    printf '%s' "$destination" > "$FAKE_CLONE_PATH_FILE"
    mkdir -p "$destination/backend" "$destination/examples"
    : > "$destination/backend/Cargo.toml"
    printf '%s\n' '# trusted test init' > "$destination/examples/init.bash"
    exit 0
fi
if [ "$1" = "-C" ]; then
    shift
    repository=$1
    shift
    if [ "$1" = "fetch" ] || [ "$1" = "checkout" ]; then
        exit 0
    fi
fi
exit 2
''',
        )
        self.write_tool(
            "cargo",
            r'''#!/bin/sh
printf 'cargo %s\n' "$*" >> "$FAKE_LOG"
if [ "$FAKE_BUILD_FAIL" = "1" ]; then
    exit 1
fi
target_dir=
explicit_target=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target-dir)
            shift
            target_dir=$1
            ;;
        --target)
            shift
            explicit_target=$1
            ;;
    esac
    shift
done
artifact_dir="$target_dir"
if [ -n "$explicit_target" ]; then
    artifact_dir="$artifact_dir/$explicit_target"
elif [ -n "${CARGO_BUILD_TARGET:-}" ]; then
    artifact_dir="$artifact_dir/$CARGO_BUILD_TARGET"
fi
mkdir -p "$artifact_dir/release"
printf '%s\n' 'synthetic mcp-shell binary' > "$artifact_dir/release/mcp-shell"
''',
        )
        self.write_tool(
            "openssl",
            "#!/bin/sh\n"
            "if [ \"$1\" = \"rand\" ]; then\n"
            "    printf '%064d' 0\n"
            "    exit 0\n"
            "fi\n"
            "exec " + shlex.quote(self.real_openssl) + r''' "$@"
''',
        )
        self.write_tool(
            "rustc",
            '#!/bin/sh\nif [ "$1" = "--print" ] && [ "$2" = "host-tuple" ]; then\nprintf "%s\\n" "$FAKE_RUSTC_HOST"\nelse\nexit 2\nfi\n',
        )
        self.write_tool(
            "systemctl",
            r'''#!/bin/sh
printf 'systemctl %s\n' "$*" >> "$FAKE_LOG"
[ "$1" = "--user" ] || exit 2
shift
case "$1" in
    show-environment)
        [ "$FAKE_MANAGER_AVAILABLE" = "1" ]
        exit $?
        ;;
    daemon-reload)
        exit 0
        ;;
    enable)
        exit 0
        ;;
    restart)
        if [ "$FAKE_RESTART_FAIL" = "1" ]; then
            exit 1
        fi
        printf '%s' "$FAKE_STARTUP_PID" > "$FAKE_SERVICE_STATE"
        : > "$FAKE_SHOW_COUNT"
        exit 0
        ;;
    show)
        count=0
        if [ -f "$FAKE_SHOW_COUNT" ]; then
            count=$(cat "$FAKE_SHOW_COUNT")
        fi
        count=$((count + 1))
        printf '%s' "$count" > "$FAKE_SHOW_COUNT"
        if [ "$FAKE_PID_MODE" = "transient" ] && [ "$count" -gt 1 ]; then
            : > "$FAKE_SERVICE_STATE"
        fi
        if [ -s "$FAKE_SERVICE_STATE" ]; then
            pid=$(cat "$FAKE_SERVICE_STATE")
            printf 'ActiveState=active\nMainPID=%s\n' "$pid"
        else
            printf 'ActiveState=failed\nMainPID=0\n'
        fi
        exit 0
        ;;
    stop|disable)
        : > "$FAKE_SERVICE_STATE"
        exit 0
        ;;
esac
exit 2
''',
        )
        self.write_tool(
            "loginctl",
            r'''#!/bin/sh
printf 'loginctl %s\n' "$*" >> "$FAKE_LOG"
case "$1" in
    show-user)
        if [ -s "$FAKE_LINGER_STATE" ]; then
            cat "$FAKE_LINGER_STATE"
        else
            printf 'no\n'
        fi
        exit 0
        ;;
    enable-linger)
        if [ "$FAKE_LINGER_FAIL" = "1" ]; then
            exit 1
        fi
        printf 'yes\n' > "$FAKE_LINGER_STATE"
        exit 0
        ;;
esac
exit 2
''',
        )
        self.write_tool("cc", "#!/bin/sh\nexit 0\n")
        self.write_tool("sleep", "#!/bin/sh\nexit 0\n")

    def write_tool(self, name: str, content: str) -> None:
        path = self.fake_bin / name
        path.write_text(content, encoding="utf-8")
        path.chmod(0o755)

    def run_installer(self, ref: str = "main", *, ubuntu: bool = True) -> subprocess.CompletedProcess[str]:
        platform_result = "0" if ubuntu else "1"
        command = (
            'source "$1"; '
            f'ubuntu_release_id() {{ return {platform_result}; }}; '
            'install_linux_for_home "$2" "$3"'
        )
        return subprocess.run(
            ["/bin/bash", "-c", command, "installer-test", str(INSTALLER), str(self.home), ref],
            env=self.env.copy(),
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )

    def run_stdin_installer(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["/bin/bash", "-s", "--", *arguments],
            input=INSTALLER.read_text(encoding="utf-8"),
            env=os.environ.copy(),
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )

    def run_bash_c_installer(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["/bin/bash", "-c", INSTALLER.read_text(encoding="utf-8"), "installer-test", *arguments],
            env=os.environ.copy(),
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )

    def read_log(self) -> str:
        return self.log.read_text(encoding="utf-8") if self.log.exists() else ""

    def assert_temporary_clone_removed(self) -> None:
        clone_path = Path(self.clone_path.read_text(encoding="utf-8"))
        self.assertFalse(clone_path.exists())
        self.assertEqual(list(self.base.glob("mcp-shell-install.*")), [])

    def test_help_stdin_error_and_source_guard(self) -> None:
        help_result = subprocess.run(
            [str(INSTALLER), "--help"], capture_output=True, text=True, timeout=5, check=False
        )
        self.assertEqual(help_result.returncode, 0)
        self.assertIn("--ref REF", help_result.stdout)

        stdin_help = self.run_stdin_installer("--help")
        self.assertEqual(stdin_help.returncode, 0)
        self.assertIn("Usage: linux-install.sh", stdin_help.stdout)

        stdin_error = self.run_stdin_installer("--unknown")
        self.assertNotEqual(stdin_error.returncode, 0)
        self.assertIn("unknown argument: --unknown", stdin_error.stderr)
        self.assertIn("Usage: linux-install.sh", stdin_error.stderr)

        bash_c_help = self.run_bash_c_installer("--help")
        self.assertEqual(bash_c_help.returncode, 0)
        self.assertIn("Usage: linux-install.sh", bash_c_help.stdout)
        sourced = subprocess.run(
            ["/bin/bash", "-c", 'source "$1"; printf "source-ok\\n"', "source-test", str(INSTALLER)],
            env=os.environ.copy(), capture_output=True, text=True, timeout=5, check=False,
        )
        self.assertEqual(sourced.returncode, 0)
        self.assertEqual(sourced.stdout, "source-ok\n")

    def test_install_writes_private_configuration_unit_and_summary(self) -> None:
        result = self.run_installer("refs/pull/16/merge")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn(GENERATED_TOKEN, result.stdout + result.stderr)
        self.assert_temporary_clone_removed()

        config_dir = self.home / ".config/mcp-shell"
        config = tomllib.loads((config_dir / "server.toml").read_text(encoding="utf-8"))
        self.assertEqual(config["allow_rules"], ["gh *", "az *"])
        self.assertEqual(config["working_directory"], str(self.home))
        self.assertEqual(config["initialization_file"], str(config_dir / "init.bash"))
        self.assertEqual((config_dir / "init.bash").read_text(encoding="utf-8"), "# trusted test init\n")
        self.assertEqual((config_dir / "server.toml").stat().st_mode & 0o777, 0o600)
        self.assertEqual((config_dir / "init.bash").stat().st_mode & 0o777, 0o600)

        token = (config_dir / "client-token").read_text(encoding="ascii")
        digest = hashlib.sha256(token.encode("ascii")).hexdigest()
        self.assertEqual(token, GENERATED_TOKEN)
        self.assertEqual((config_dir / "client-token").stat().st_mode & 0o777, 0o600)
        self.assertEqual((config_dir / "server-token.sha256").stat().st_mode & 0o777, 0o600)
        self.assertEqual((config_dir / "server-token.sha256").read_text(encoding="ascii"), digest)
        self.assertEqual(config_dir.stat().st_mode & 0o777, 0o700)

        unit_path = self.home / ".config/systemd/user" / UNIT
        unit = unit_path.read_text(encoding="utf-8")
        self.assertEqual(unit_path.stat().st_mode & 0o777, 0o600)
        self.assertEqual((self.home / ".local/bin/mcp-shell").stat().st_mode & 0o777, 0o755)
        def escape_unit_value(value: str) -> str:
            return value.replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%")

        expected_start = (
            f'ExecStart=:/usr/bin/env -- "{escape_unit_value(str(self.home / ".local/bin/mcp-shell"))}" '
            f'--config "{escape_unit_value(str(config_dir / "server.toml"))}"'
        )
        self.assertIn(expected_start, unit)
        self.assertIn(f'Environment="HOME={escape_unit_value(str(self.home))}"', unit)
        self.assertIn(f'"PATH={escape_unit_value(self.env["PATH"])}"', unit)
        self.assertIn("Type=exec", unit)
        self.assertIn("Restart=always", unit)
        self.assertIn("RestartSec=1", unit)
        self.assertIn("KillMode=control-group", unit)
        self.assertIn("TimeoutStopSec=15", unit)
        self.assertIn("UnsetEnvironment=MCP_SHELL_TOKEN", unit)
        self.assertIn("MCP_SHELL_TOKEN_SHA256", unit)
        self.assertNotIn(GENERATED_TOKEN, unit)
        self.assertNotIn("client-token", unit)
        self.assertIn("WantedBy=default.target", unit)
        self.assertIn(f"journalctl --user -u {UNIT}", result.stdout)
        self.assertIn("Installed files:", result.stdout)
        self.assertIn(str(self.home / ".local/bin/mcp-shell"), result.stdout)
        self.assertIn("origin -- +refs/pull/16/merge", self.read_log())
        log = self.read_log()
        self.assertLess(log.index("cargo build"), log.index("loginctl enable-linger"))
        self.assertLess(log.index("loginctl enable-linger"), log.index("systemctl --user restart"))
        self.assertIn("systemctl --user daemon-reload", log)
        self.assertIn("systemctl --user enable mcp-shell.service", log)
        self.assertIn("systemctl --user restart mcp-shell.service", log)

        systemd_analyze = shutil.which("systemd-analyze")
        if systemd_analyze is not None:
            verify = subprocess.run(
                [systemd_analyze, "verify", str(unit_path)],
                capture_output=True, text=True, timeout=10, check=False,
            )
            self.assertEqual(verify.returncode, 0, verify.stderr)

    def test_build_uses_compiler_host_when_cargo_target_is_set(self) -> None:
        self.env["CARGO_BUILD_TARGET"] = "wasm32-unknown-unknown"
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--target x86_64-unknown-linux-gnu", self.read_log())

    def test_rerun_preserves_config_init_and_client_token(self) -> None:
        first = self.run_installer()
        self.assertEqual(first.returncode, 0, first.stderr)
        config_dir = self.home / ".config/mcp-shell"
        config_path = config_dir / "server.toml"
        init_path = config_dir / "init.bash"
        token_path = config_dir / "client-token"
        config_bytes = b'# operator configuration\nallow_rules = ["gh pr view*"]\n'
        init_bytes = b"export PATH=/custom/bin:$PATH\n"
        token_bytes = b"existing-token-123!"
        config_path.write_bytes(config_bytes)
        init_path.write_bytes(init_bytes)
        token_path.write_bytes(token_bytes)
        token_path.chmod(0o644)

        second = self.run_installer("refs/heads/main")
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(config_path.read_bytes(), config_bytes)
        self.assertEqual(init_path.read_bytes(), init_bytes)
        self.assertEqual(token_path.read_bytes(), token_bytes)
        self.assertEqual(token_path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(
            (config_dir / "server-token.sha256").read_text(encoding="ascii"),
            hashlib.sha256(token_bytes).hexdigest(),
        )
        self.assertIn("systemctl --user restart mcp-shell.service", self.read_log())
        self.assertEqual(self.read_log().count("loginctl enable-linger 1000"), 1)
        self.assert_temporary_clone_removed()

    def test_build_failure_preserves_existing_files_and_service(self) -> None:
        config_dir = self.home / ".config/mcp-shell"
        unit_dir = self.home / ".config/systemd/user"
        bin_dir = self.home / ".local/bin"
        config_dir.mkdir(parents=True)
        unit_dir.mkdir(parents=True)
        bin_dir.mkdir(parents=True)
        config_path = config_dir / "server.toml"
        unit_path = unit_dir / UNIT
        binary_path = bin_dir / "mcp-shell"
        config_path.write_text("existing config\n", encoding="utf-8")
        unit_path.write_text("existing unit\n", encoding="utf-8")
        binary_path.write_text("existing binary\n", encoding="utf-8")
        self.service_state.write_text("1222", encoding="ascii")
        self.env["FAKE_BUILD_FAIL"] = "1"

        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("build failed", result.stderr)
        self.assertEqual(config_path.read_text(encoding="utf-8"), "existing config\n")
        self.assertEqual(unit_path.read_text(encoding="utf-8"), "existing unit\n")
        self.assertEqual(binary_path.read_text(encoding="utf-8"), "existing binary\n")
        self.assertNotIn("systemctl --user restart", self.read_log())
        self.assertNotIn("loginctl enable-linger", self.read_log())
        self.assert_temporary_clone_removed()

    def test_invalid_existing_tokens_never_start_service(self) -> None:
        config_dir = self.home / ".config/mcp-shell"
        config_dir.mkdir(parents=True)
        token_path = config_dir / "client-token"
        for token_bytes in (b"", b"valid-token\n", b"before\x00after"):
            with self.subTest(token_bytes=token_bytes):
                self.log.unlink(missing_ok=True)
                self.service_state.unlink(missing_ok=True)
                token_path.write_bytes(token_bytes)
                result = self.run_installer()
                self.assertNotEqual(result.returncode, 0)
                invalid_text = token_bytes.decode("ascii", errors="ignore")
                if invalid_text:
                    self.assertNotIn(invalid_text, result.stdout + result.stderr)
                self.assertNotIn("systemctl --user restart", self.read_log())
                self.assert_temporary_clone_removed()

    def test_root_non_ubuntu_and_missing_user_manager_fail_before_clone(self) -> None:
        non_ubuntu = self.run_installer(ubuntu=False)
        self.assertNotEqual(non_ubuntu.returncode, 0)
        self.assertIn("requires Ubuntu Linux", non_ubuntu.stderr)
        self.assertFalse(self.clone_path.exists())

        self.env["FAKE_UID"] = "0"
        root_result = self.run_installer()
        self.assertNotEqual(root_result.returncode, 0)
        self.assertIn("non-root", root_result.stderr)
        self.assertFalse(self.clone_path.exists())

        self.env["FAKE_UID"] = "1000"
        self.env["FAKE_MANAGER_AVAILABLE"] = "0"
        no_manager = self.run_installer()
        self.assertNotEqual(no_manager.returncode, 0)
        self.assertIn("normal Ubuntu user login session", no_manager.stderr)
        self.assertFalse(self.clone_path.exists())

    def test_linger_failure_prevents_service_change(self) -> None:
        self.env["FAKE_LINGER_FAIL"] = "1"
        unit_dir = self.home / ".config/systemd/user"
        unit_dir.mkdir(parents=True)
        unit_path = unit_dir / UNIT
        unit_path.write_text("existing unit\n", encoding="utf-8")

        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("could not enable this user's linger setting", result.stderr)
        self.assertEqual(unit_path.read_text(encoding="utf-8"), "existing unit\n")
        self.assertNotIn("systemctl --user restart", self.read_log())
        self.assert_temporary_clone_removed()

    def test_restart_failure_stops_and_disables_owned_service(self) -> None:
        self.env["FAKE_RESTART_FAIL"] = "1"
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("journalctl --user -u mcp-shell.service", result.stderr)
        self.assertIn("systemctl --user stop mcp-shell.service", self.read_log())
        self.assertIn("systemctl --user disable mcp-shell.service", self.read_log())
        self.assert_temporary_clone_removed()

    def test_unstable_service_pid_is_stopped_and_disabled(self) -> None:
        self.env["FAKE_PID_MODE"] = "transient"
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not start", result.stderr)
        self.assertIn("systemctl --user stop mcp-shell.service", self.read_log())
        self.assertIn("systemctl --user disable mcp-shell.service", self.read_log())
        self.assert_temporary_clone_removed()


if __name__ == "__main__":
    unittest.main()
