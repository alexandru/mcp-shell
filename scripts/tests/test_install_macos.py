"""Exercise the macOS installer with an isolated home and fake platform tools."""

import hashlib
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "install-macos.sh"
LABEL = "io.github.alexandru.mcp-shell"
GENERATED_TOKEN = "0" * 64


class InstallerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory(prefix="mcp-installer-test-")
        self.addCleanup(self.temporary_directory.cleanup)
        self.base = Path(self.temporary_directory.name)
        self.home = self.base / "Home space 'single' \"double\" & back\\slash"
        self.home.mkdir()
        self.fake_bin = self.base / "fake-bin"
        self.fake_bin.mkdir()
        self.log = self.base / "commands.log"
        self.state = self.base / "launchctl-state"
        self.clone_path = self.base / "clone-path"
        self.print_count = self.base / "launchctl-print-count"
        self.real_openssl = shutil.which("openssl")
        if self.real_openssl is None:
            self.fail("openssl is required to exercise the installer digest path")
        self.env = {
            "PATH": f"{self.fake_bin}:{os.defpath}",
            "TMPDIR": str(self.base),
            "FAKE_LOG": str(self.log),
            "FAKE_LAUNCHCTL_STATE": str(self.state),
            "FAKE_CLONE_PATH_FILE": str(self.clone_path),
            "FAKE_PRINT_COUNT": str(self.print_count),
            "FAKE_UID": "501",
            "FAKE_LABEL": LABEL,
            "FAKE_GUI_PRESENT": "1",
            "FAKE_STARTUP_PID": "1301",
            "FAKE_BUILD_FAIL": "0",
            "FAKE_BOOTSTRAP_FAIL": "0",
            "FAKE_PID_MODE": "stable",
            "FAKE_PLATFORM": "Darwin",
            "FAKE_RUSTC_HOST": "aarch64-apple-darwin",
            "REAL_OPENSSL": self.real_openssl,
        }
        self.write_fake_tools()

    def write_fake_tools(self) -> None:
        self.write_tool("uname", '#!/bin/sh\nprintf "%s\\n" "$FAKE_PLATFORM"\n')
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
target=
explicit_target=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --target-dir)
            shift
            target=$1
            ;;
        --target)
            shift
            explicit_target=$1
            ;;
    esac
    shift
done
artifact_dir="$target"
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
            "plutil",
            r'''#!/bin/sh
printf 'plutil %s\n' "$*" >> "$FAKE_LOG"
[ "$1" = "-lint" ] && [ -f "$2" ]
''',
        )
        self.write_tool(
            "launchctl",
            r'''#!/bin/sh
printf 'launchctl %s\n' "$*" >> "$FAKE_LOG"
command=$1
shift
target=$1
service="gui/$FAKE_UID/$FAKE_LABEL"
case "$command" in
    print)
        if [ "$target" = "gui/$FAKE_UID" ]; then
            [ "$FAKE_GUI_PRESENT" = "1" ]
            exit $?
        fi
        if [ "$target" = "$service" ] && [ -s "$FAKE_LAUNCHCTL_STATE" ]; then
            pid=$(cat "$FAKE_LAUNCHCTL_STATE")
            if [ "$FAKE_PID_MODE" = "transient" ]; then
                count=0
                if [ -f "$FAKE_PRINT_COUNT" ]; then
                    count=$(cat "$FAKE_PRINT_COUNT")
                fi
                count=$((count + 1))
                printf '%s' "$count" > "$FAKE_PRINT_COUNT"
                if [ "$count" -gt 1 ]; then
                    : > "$FAKE_LAUNCHCTL_STATE"
                    exit 1
                fi
            fi
            printf 'state = running\npid = %s\n' "$pid"
            exit 0
        fi
        exit 1
        ;;
    bootout)
        : > "$FAKE_LAUNCHCTL_STATE"
        ;;
    enable)
        ;;
    bootstrap)
        if [ "$FAKE_BOOTSTRAP_FAIL" = "1" ]; then
            exit 1
        fi
        printf '%s' "$FAKE_STARTUP_PID" > "$FAKE_LAUNCHCTL_STATE"
        ;;
    *)
        exit 2
        ;;
esac
exit 0
''',
        )
        self.write_tool(
            "rustc",
            '#!/bin/sh\nif [ "$1" = "--print" ] && [ "$2" = "host-tuple" ]; then\nprintf "%s\\n" "$FAKE_RUSTC_HOST"\nelse\nexit 2\nfi\n',
        )
        self.write_tool("cc", "#!/bin/sh\nexit 0\n")
        self.write_tool("sleep", "#!/bin/sh\nexit 0\n")

    def write_tool(self, name: str, content: str) -> None:
        path = self.fake_bin / name
        path.write_text(content, encoding="utf-8")
        path.chmod(0o755)

    def run_installer(self, ref: str = "main") -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "/bin/bash",
                "-c",
                'source "$1"; install_macos_for_home "$2" "$3"',
                "installer-test",
                str(INSTALLER),
                str(self.home),
                ref,
            ],
            env=self.env.copy(),
            capture_output=True,
            text=True,
            timeout=15,
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
            [
                "/bin/bash",
                "-c",
                INSTALLER.read_text(encoding="utf-8"),
                "installer-test",
                *arguments,
            ],
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

    def test_help_does_not_require_macos_or_installation(self) -> None:
        result = subprocess.run(
            [str(INSTALLER), "--help"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        self.assertEqual(result.returncode, 0)
        self.assertIn("--ref REF", result.stdout)

    def test_stdin_help_executes_main(self) -> None:
        result = self.run_stdin_installer("--help")

        self.assertEqual(result.returncode, 0)
        self.assertIn("Usage: install-macos.sh", result.stdout)
        self.assertIn("--ref REF", result.stdout)

    def test_stdin_bad_argument_returns_usage_error(self) -> None:
        result = self.run_stdin_installer("--unknown")

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown argument: --unknown", result.stderr)
        self.assertIn("Usage: install-macos.sh", result.stderr)

    def test_bash_c_executes_main_and_sourcing_does_not(self) -> None:
        bash_c_result = self.run_bash_c_installer("--help")
        self.assertEqual(bash_c_result.returncode, 0)
        self.assertIn("Usage: install-macos.sh", bash_c_result.stdout)

        sourced_result = subprocess.run(
            ["/bin/bash", "-c", 'source "$1"; printf "source-ok\\n"', "source-test", str(INSTALLER)],
            env=os.environ.copy(),
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        self.assertEqual(sourced_result.returncode, 0)
        self.assertEqual(sourced_result.stdout, "source-ok\n")
        self.assertEqual(sourced_result.stderr, "")

    def test_readme_install_command_propagates_curl_failure(self) -> None:
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        code_block = re.search(r"```sh\n(.*?)\n```", readme, re.DOTALL)
        self.assertIsNotNone(code_block)
        self.assertIsNotNone(code_block and code_block.group(1).strip())
        command = code_block.group(1).strip()
        prefix = "/bin/bash -o pipefail -c '"
        self.assertTrue(command.startswith(prefix))
        self.assertTrue(command.endswith("'"))
        bare_pipeline = command[len(prefix):-1]

        self.write_tool(
            "curl",
            r'''#!/bin/sh
if [ "$FAKE_CURL_STATUS" -ne 0 ]; then
    exit "$FAKE_CURL_STATUS"
fi
printf '%s\n' "printf 'pipeline-ran\\n'"
''',
        )
        env = os.environ.copy()
        env["PATH"] = f"{self.fake_bin}:{env.get('PATH', os.defpath)}"
        env["FAKE_CURL_STATUS"] = "0"
        success = subprocess.run(
            ["/bin/bash", "-c", command],
            env=env,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        self.assertEqual(success.returncode, 0)
        self.assertEqual(success.stdout, "pipeline-ran\n")

        env["FAKE_CURL_STATUS"] = "22"
        bare_failure = subprocess.run(
            ["/bin/bash", "-c", bare_pipeline],
            env=env,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        self.assertEqual(bare_failure.returncode, 0)
        self.assertEqual(bare_failure.stdout, "")

        wrapped_failure = subprocess.run(
            ["/bin/bash", "-c", command],
            env=env,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
        self.assertEqual(wrapped_failure.returncode, 22)
        self.assertEqual(wrapped_failure.stdout, "")

    def test_installs_escaped_config_plist_and_private_files(self) -> None:
        result = self.run_installer("refs/pull/11/merge")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn(GENERATED_TOKEN, result.stdout + result.stderr)
        self.assert_temporary_clone_removed()

        config_dir = self.home / ".config/mcp-shell"
        server_config = (config_dir / "server.toml").read_text(encoding="utf-8")
        import tomllib

        config = tomllib.loads(server_config)
        self.assertEqual(config["bind"], "0.0.0.0:8090")
        self.assertEqual(
            config["allowed_hosts"],
            ["127.0.0.1:8090", "localhost:8090", "host.docker.internal:8090"],
        )
        self.assertEqual(config["allow_rules"], ["gh *", "az *"])
        self.assertEqual(
            config["description"],
            "Use this tool for GitHub and Azure commands with authentication from the host user.",
        )
        self.assertEqual(config["working_directory"], str(self.home))
        self.assertEqual(config["initialization_file"], str(config_dir / "init.bash"))
        self.assertEqual(
            (config_dir / "init.bash").read_text(encoding="utf-8"),
            "# trusted test init\n",
        )

        token = (config_dir / "client-token").read_text(encoding="ascii")
        digest = hashlib.sha256(token.encode("ascii")).hexdigest()
        self.assertEqual(token, GENERATED_TOKEN)
        self.assertEqual((config_dir / "client-token").stat().st_mode & 0o777, 0o600)
        self.assertEqual((config_dir / "server-token.sha256").stat().st_mode & 0o777, 0o600)
        self.assertEqual(
            (config_dir / "server-token.sha256").read_text(encoding="ascii"),
            digest,
        )
        self.assertEqual(config_dir.stat().st_mode & 0o777, 0o700)
        self.assertEqual((self.home / "Library/Logs/mcp-shell").stat().st_mode & 0o777, 0o700)

        plist_path = self.home / f"Library/LaunchAgents/{LABEL}.plist"
        plist = plistlib.loads(plist_path.read_bytes())
        self.assertEqual(
            plist["ProgramArguments"],
            [str(self.home / ".local/bin/mcp-shell"), "--config", str(config_dir / "server.toml")],
        )
        self.assertEqual(
            plist["EnvironmentVariables"],
            {"HOME": str(self.home), "PATH": self.env["PATH"], "MCP_SHELL_TOKEN_SHA256": digest},
        )
        self.assertEqual(plist["Label"], LABEL)
        self.assertTrue(plist["RunAtLoad"])
        self.assertTrue(plist["KeepAlive"])
        self.assertEqual(plist_path.stat().st_mode & 0o777, 0o600)
        self.assertEqual((self.home / ".local/bin/mcp-shell").stat().st_mode & 0o777, 0o755)
        self.assertIn("+refs/pull/11/merge", self.read_log())
        log = self.read_log()
        self.assertLess(log.index("cargo build"), log.index(f"launchctl print gui/501/{LABEL}"))
        self.assertGreaterEqual(log.count(f"launchctl print gui/501/{LABEL}"), 3)

    def test_supplied_server_hash_is_used_without_client_token(self) -> None:
        supplied_hash = "aBcDeF0123456789" * 4
        self.env["MCP_SHELL_TOKEN_SHA256"] = supplied_hash
        self.env["MCP_SHELL_TOKEN"] = "ignored-client-token"
        self.write_tool("openssl", "#!/bin/sh\nexit 1\n")

        result = self.run_installer()

        self.assertEqual(result.returncode, 0, result.stderr)
        config_dir = self.home / ".config/mcp-shell"
        verifier = config_dir / "server-token.sha256"
        self.assertEqual(verifier.read_text(encoding="ascii"), supplied_hash)
        self.assertEqual(verifier.stat().st_mode & 0o777, 0o600)
        self.assertFalse((config_dir / "client-token").exists())
        plist_path = self.home / f"Library/LaunchAgents/{LABEL}.plist"
        plist = plistlib.loads(plist_path.read_bytes())
        self.assertEqual(plist["EnvironmentVariables"]["MCP_SHELL_TOKEN_SHA256"], supplied_hash)
        self.assertNotIn("Client token:", result.stdout)
        self.assertNotIn(supplied_hash, result.stdout + result.stderr)
        self.assertNotIn(self.env["MCP_SHELL_TOKEN"], result.stdout + result.stderr)

    def test_supplied_server_hash_preserves_invalid_client_token(self) -> None:
        config_dir = self.home / ".config/mcp-shell"
        config_dir.mkdir(parents=True)
        client_token = config_dir / "client-token"
        client_token.write_bytes(b"invalid token\n")
        client_token.chmod(0o644)
        self.env["MCP_SHELL_TOKEN_SHA256"] = "a" * 64

        result = self.run_installer()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(client_token.read_bytes(), b"invalid token\n")
        self.assertEqual(client_token.stat().st_mode & 0o777, 0o644)
        self.assertNotIn("Client token:", result.stdout)

    def test_invalid_supplied_server_hash_never_starts_service(self) -> None:
        for supplied_hash in ("", "a" * 63, "g" * 64, "a" * 63 + "\n"):
            with self.subTest(supplied_hash=repr(supplied_hash)):
                self.log.unlink(missing_ok=True)
                self.env["MCP_SHELL_TOKEN_SHA256"] = supplied_hash
                result = self.run_installer()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("MCP_SHELL_TOKEN_SHA256", result.stderr)
                if supplied_hash:
                    self.assertNotIn(supplied_hash, result.stdout + result.stderr)
                self.assertFalse((self.home / ".config/mcp-shell/client-token").exists())
                self.assertNotIn("launchctl bootstrap", self.read_log())

    def test_client_environment_token_does_not_change_generated_token(self) -> None:
        self.env["MCP_SHELL_TOKEN"] = "ignored-client-token"

        result = self.run_installer()

        self.assertEqual(result.returncode, 0, result.stderr)
        config_dir = self.home / ".config/mcp-shell"
        self.assertEqual((config_dir / "client-token").read_text(encoding="ascii"), GENERATED_TOKEN)
        self.assertEqual(
            (config_dir / "server-token.sha256").read_text(encoding="ascii"),
            hashlib.sha256(GENERATED_TOKEN.encode("ascii")).hexdigest(),
        )

    def test_build_uses_compiler_host_when_cargo_target_environment_is_set(self) -> None:
        self.env["CARGO_BUILD_TARGET"] = "wasm32-unknown-unknown"

        result = self.run_installer()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--target aarch64-apple-darwin", self.read_log())
        self.assertTrue((self.home / ".local/bin/mcp-shell").is_file())

    def test_existing_files_are_preserved_on_rerun(self) -> None:
        first = self.run_installer()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertIn("origin -- +main", self.read_log())

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
        self.env["FAKE_STARTUP_PID"] = "1302"

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
        self.assertIn("launchctl bootout", self.read_log())
        self.assertIn("launchctl bootstrap gui/501", self.read_log())

        commit_id = "a" * 40
        self.env["FAKE_STARTUP_PID"] = "1303"
        third = self.run_installer(commit_id)
        self.assertEqual(third.returncode, 0, third.stderr)
        self.assertIn(f"origin -- +{commit_id}", self.read_log())
        self.assert_temporary_clone_removed()

    def test_build_failure_preserves_existing_service_and_files(self) -> None:
        config_dir = self.home / ".config/mcp-shell"
        bin_dir = self.home / ".local/bin"
        agent_dir = self.home / "Library/LaunchAgents"
        config_dir.mkdir(parents=True)
        bin_dir.mkdir(parents=True)
        agent_dir.mkdir(parents=True)
        existing_config = config_dir / "server.toml"
        existing_binary = bin_dir / "mcp-shell"
        existing_plist = agent_dir / f"{LABEL}.plist"
        existing_config.write_text("existing config\n", encoding="utf-8")
        existing_binary.write_text("existing binary\n", encoding="utf-8")
        existing_plist.write_text("existing plist\n", encoding="utf-8")
        self.state.write_text("1222", encoding="ascii")
        self.env["FAKE_BUILD_FAIL"] = "1"

        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("build failed", result.stderr)
        self.assertEqual(existing_config.read_text(encoding="utf-8"), "existing config\n")
        self.assertEqual(existing_binary.read_text(encoding="utf-8"), "existing binary\n")
        self.assertEqual(existing_plist.read_text(encoding="utf-8"), "existing plist\n")
        self.assertEqual(self.state.read_text(encoding="ascii"), "1222")
        self.assertNotIn("launchctl bootout", self.read_log())
        self.assertNotIn("launchctl bootstrap", self.read_log())
        self.assert_temporary_clone_removed()

    def test_unstable_service_pid_is_booted_out(self) -> None:
        self.env["FAKE_PID_MODE"] = "transient"
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not start", result.stderr)
        self.assertEqual(self.state.read_text(encoding="ascii"), "")
        self.assert_temporary_clone_removed()

    def test_invalid_existing_client_tokens_never_start_service(self) -> None:
        config_dir = self.home / ".config/mcp-shell"
        config_dir.mkdir(parents=True)
        token_path = config_dir / "client-token"

        for token_bytes in (b"", b"valid-token\n", b"before\x00after"):
            with self.subTest(token_bytes=token_bytes):
                self.log.unlink(missing_ok=True)
                self.state.unlink(missing_ok=True)
                token_path.write_bytes(token_bytes)

                result = self.run_installer()

                self.assertNotEqual(result.returncode, 0)
                invalid_text = token_bytes.decode("ascii", errors="ignore")
                if invalid_text:
                    self.assertNotIn(invalid_text, result.stdout + result.stderr)
                self.assertNotIn("launchctl bootstrap", self.read_log())
                self.assert_temporary_clone_removed()

    def test_rejects_non_macos_root_and_missing_gui_domain_before_clone(self) -> None:
        uname = self.fake_bin / "uname"
        uid = self.fake_bin / "id"

        uname.write_text('#!/bin/sh\nprintf "Linux\\n"\n', encoding="utf-8")
        uname.chmod(0o755)
        non_macos = self.run_installer()
        self.assertNotEqual(non_macos.returncode, 0)
        self.assertIn("requires macOS", non_macos.stderr)
        self.assertFalse(self.clone_path.exists())

        uname.write_text('#!/bin/sh\nprintf "Darwin\\n"\n', encoding="utf-8")
        uid.write_text('#!/bin/sh\nprintf "0\\n"\n', encoding="utf-8")
        uid.chmod(0o755)
        root_result = self.run_installer()
        self.assertNotEqual(root_result.returncode, 0)
        self.assertIn("non-root", root_result.stderr)
        self.assertFalse(self.clone_path.exists())

        uid.write_text('#!/bin/sh\nprintf "501\\n"\n', encoding="utf-8")
        uid.chmod(0o755)
        self.env["FAKE_GUI_PRESENT"] = "0"
        no_gui = self.run_installer()
        self.assertNotEqual(no_gui.returncode, 0)
        self.assertIn("GUI launchctl domain", no_gui.stderr)
        self.assertFalse(self.clone_path.exists())


if __name__ == "__main__":
    unittest.main()
