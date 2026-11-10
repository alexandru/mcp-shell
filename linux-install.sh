#!/bin/bash

MCP_SHELL_REPOSITORY="https://github.com/alexandru/mcp-shell.git"
MCP_SHELL_UNIT="mcp-shell.service"

installer_error() {
    printf 'mcp-shell installer: %s\n' "$1" >&2
}

toml_escape() {
    local value="$1"
    local escaped=""
    local character
    local index

    case "$value" in
        *[[:cntrl:]]*) return 1 ;;
    esac

    for ((index = 0; index < ${#value}; index++)); do
        character=${value:index:1}
        case "$character" in
            \\) escaped="${escaped}\\\\" ;;
            '"') escaped="${escaped}\\\"" ;;
            *) escaped="${escaped}${character}" ;;
        esac
    done
    printf '%s' "$escaped"
}

systemd_c_quote() {
    local value="$1"
    local escaped=""
    local character
    local index

    case "$value" in
        *[[:cntrl:]]*) return 1 ;;
    esac

    for ((index = 0; index < ${#value}; index++)); do
        character=${value:index:1}
        case "$character" in
            \\) escaped="${escaped}\\\\" ;;
            '"') escaped="${escaped}\\\"" ;;
            '%') escaped="${escaped}%%" ;;
            *) escaped="${escaped}${character}" ;;
        esac
    done
    printf '"%s"' "$escaped"
}

valid_header_token() {
    local token="$1"
    local character
    local index
    local code
    local LC_ALL=C

    [[ -n "$token" ]] || return 1
    for ((index = 0; index < ${#token}; index++)); do
        character=${token:index:1}
        printf -v code '%d' "'$character"
        if ((code < 33 || code > 126)); then
            return 1
        fi
    done
    return 0
}

valid_ref() {
    local ref="$1"

    if [[ "$ref" =~ ^[[:xdigit:]]{40}([[:xdigit:]]{24})?$ ]]; then
        return 0
    fi
    case "$ref" in
        refs/*) git check-ref-format "$ref" >/dev/null 2>&1 ;;
        *) git check-ref-format "refs/heads/$ref" >/dev/null 2>&1 ;;
    esac
}

service_pid() {
    local target="$1"
    local status
    local line
    local active=""
    local pid

    status="$(systemctl --user show --property=ActiveState --property=MainPID "$target" 2>/dev/null)" || return 1
    while IFS= read -r line; do
        case "$line" in
            ActiveState=*) active=${line#ActiveState=} ;;
            MainPID=*) pid=${line#MainPID=} ;;
        esac
    done <<EOF
$status
EOF
    [[ "$active" == active && "${pid:-}" =~ ^[0-9]+$ ]] || return 1
    ((pid > 0)) || return 1
    printf '%s' "$pid"
}

ubuntu_release_id() {
    if [[ ! -r /etc/os-release ]]; then
        return 1
    fi
    . /etc/os-release
    [[ "${ID:-}" == ubuntu ]]
}

ensure_linger() {
    local current

    if ! current="$(loginctl show-user "$uid" -p Linger --value 2>/dev/null)"; then
        installer_error "could not check this user's systemd linger setting"
        return 1
    fi
    if [[ "$current" != yes ]]; then
        if ! loginctl enable-linger "$uid" >/dev/null 2>&1; then
            installer_error "could not enable this user's linger setting; check loginctl's permission or manager error"
            return 1
        fi
    fi
    if ! current="$(loginctl show-user "$uid" -p Linger --value 2>/dev/null)" \
        || [[ "$current" != yes ]]; then
        installer_error "this user's linger setting is not enabled"
        return 1
    fi
}

install_linux_for_home() (
    set -euo pipefail
    umask 077

    if [[ "$#" -ne 2 ]]; then
        installer_error "internal installer call requires a home path and ref"
        return 2
    fi

    home_dir="$1"
    ref="$2"
    work_dir=""
    server_config_tmp=""
    init_tmp=""
    client_token_tmp=""
    verifier_tmp=""
    binary_tmp=""
    unit_tmp=""

    cleanup_installer() {
        if [[ -n "$work_dir" ]]; then
            rm -rf "$work_dir" || :
        fi
        for temporary_file in \
            "$server_config_tmp" "$init_tmp" "$client_token_tmp" \
            "$verifier_tmp" "$binary_tmp" "$unit_tmp"; do
            if [[ -n "$temporary_file" ]]; then
                rm -f "$temporary_file" || :
            fi
        done
    }

    trap cleanup_installer EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM

    if [[ "$(uname -s)" != "Linux" ]] || ! ubuntu_release_id; then
        installer_error "this installer requires Ubuntu Linux"
        return 1
    fi
    uid="$(id -u)"
    if [[ "$uid" == "0" ]]; then
        installer_error "run this installer as your non-root user"
        return 1
    fi
    case "$home_dir" in
        /*) ;;
        *) installer_error "home path must be absolute"; return 1 ;;
    esac
    if [[ ! -d "$home_dir" ]]; then
        installer_error "home path must name an existing directory"
        return 1
    fi
    for tool in git cargo rustc openssl systemctl loginctl mktemp install mkdir chmod mv rm sleep; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            installer_error "required command is missing: $tool"
            return 1
        fi
    done
    if ! command -v cc >/dev/null 2>&1 \
        && ! command -v clang >/dev/null 2>&1 \
        && ! command -v gcc >/dev/null 2>&1; then
        installer_error "a C compiler is required by the Rust build"
        return 1
    fi

    if [[ -z "$ref" ]] || ! valid_ref "$ref"; then
        installer_error "ref must be a branch, tag, commit ID, or full ref"
        return 1
    fi
    toml_home="$(toml_escape "$home_dir")" || {
        installer_error "home path contains a character that TOML cannot store"
        return 1
    }
    path_value="${PATH:-/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin}"
    config_dir="$home_dir/.config/mcp-shell"
    bin_dir="$home_dir/.local/bin"
    unit_dir="$home_dir/.config/systemd/user"
    server_config="$config_dir/server.toml"
    init_file="$config_dir/init.bash"
    client_token="$config_dir/client-token"
    verifier_file="$config_dir/server-token.sha256"
    binary_file="$bin_dir/mcp-shell"
    unit_file="$unit_dir/$MCP_SHELL_UNIT"
    escaped_home_environment="$(systemd_c_quote "HOME=$home_dir")" || {
        installer_error "home path contains a character that systemd cannot store"
        return 1
    }
    escaped_path_environment="$(systemd_c_quote "PATH=$path_value")" || {
        installer_error "PATH contains a character that systemd cannot store"
        return 1
    }
    escaped_binary="$(systemd_c_quote "$binary_file")" || {
        installer_error "binary path contains a character that systemd cannot store"
        return 1
    }
    escaped_config="$(systemd_c_quote "$server_config")" || {
        installer_error "configuration path contains a character that systemd cannot store"
        return 1
    }
    if ! systemctl --user show-environment >/dev/null 2>&1; then
        installer_error "a normal Ubuntu user login session with a systemd user manager is required"
        return 1
    fi

    if [[ "$ref" == *[[:cntrl:]]* ]]; then
        installer_error "ref contains a control character"
        return 1
    fi
    work_dir="$(mktemp -d "${TMPDIR:-/tmp}/mcp-shell-install.XXXXXX")" || {
        installer_error "could not create a temporary build directory"
        return 1
    }
    source_dir="$work_dir/source"
    if ! git clone --quiet "$MCP_SHELL_REPOSITORY" "$source_dir"; then
        installer_error "could not clone the public repository"
        return 1
    fi
    if ! git -C "$source_dir" fetch --quiet --no-tags origin -- "+$ref"; then
        installer_error "could not fetch the requested ref"
        return 1
    fi
    if ! git -C "$source_dir" checkout --quiet --detach FETCH_HEAD; then
        installer_error "could not check out the requested ref"
        return 1
    fi
    if ! native_target="$(cd "$source_dir/backend" && rustc --print host-tuple 2>/dev/null)" \
        || [[ -z "$native_target" || "$native_target" == *[!a-zA-Z0-9_-]* ]]; then
        installer_error "could not determine the Rust compiler host target"
        return 1
    fi
    if ! (cd "$source_dir/backend" && cargo build --locked --release \
        --package backend --bin mcp-shell --target "$native_target" \
        --target-dir "$work_dir/build"); then
        installer_error "the native server build failed; the installed service was not changed"
        return 1
    fi

    if ! ensure_linger; then
        return 1
    fi

    journal_command="journalctl --user -u $MCP_SHELL_UNIT"

    if ! mkdir -p "$config_dir" "$bin_dir" "$unit_dir"; then
        installer_error "could not create installation directories"
        return 1
    fi
    if ! chmod 700 "$config_dir" "$unit_dir"; then
        installer_error "could not set private directory permissions"
        return 1
    fi

    if [[ -e "$server_config" || -L "$server_config" ]]; then
        if [[ ! -f "$server_config" ]]; then
            installer_error "existing server.toml is not a regular file"
            return 1
        fi
    else
        escaped_init="$(toml_escape "$init_file")" || {
            installer_error "initialization path contains a character that TOML cannot store"
            return 1
        }
        server_config_tmp="$config_dir/.server.toml.$$"
        if ! {
            printf 'bind = "127.0.0.1:8090"\n'
            printf 'allowed_hosts = ["127.0.0.1:8090", "localhost:8090", "host.docker.internal:8090", "host.docker.internal:8443"]\n'
            printf 'allowed_origins = []\n'
            printf 'allow_rules = ["gh *", "az *"]\n'
            printf 'description = "Use this tool for GitHub and Azure commands with authentication from the host user."\n'
            printf 'initialization_file = "%s"\n' "$escaped_init"
            printf 'working_directory = "%s"\n' "$toml_home"
        } > "$server_config_tmp"; then
            installer_error "could not write server.toml"
            return 1
        fi
        if ! chmod 600 "$server_config_tmp" || ! mv -f "$server_config_tmp" "$server_config"; then
            installer_error "could not install server.toml"
            return 1
        fi
        server_config_tmp=""
    fi

    if [[ -e "$init_file" || -L "$init_file" ]]; then
        if [[ ! -f "$init_file" ]]; then
            installer_error "existing init.bash is not a regular file"
            return 1
        fi
    else
        init_tmp="$config_dir/.init.bash.$$"
        if ! install -m 600 "$source_dir/examples/init.bash" "$init_tmp" \
            || ! mv -f "$init_tmp" "$init_file"; then
            installer_error "could not install init.bash"
            return 1
        fi
        init_tmp=""
    fi

    if [[ -e "$client_token" || -L "$client_token" ]]; then
        if [[ ! -f "$client_token" ]]; then
            installer_error "existing client-token is not a regular file"
            return 1
        fi
        if IFS= read -r raw_token < "$client_token"; then
            installer_error "existing client-token must be one line with no trailing newline"
            return 1
        fi
        if [[ -z "$raw_token" ]]; then
            installer_error "existing client-token is empty"
            return 1
        fi
        token_bytes="$(LC_ALL=C wc -c < "$client_token")"
        token_bytes="${token_bytes//[[:space:]]/}"
        if [[ "$token_bytes" != "${#raw_token}" ]]; then
            installer_error "existing client-token contains unsupported bytes"
            return 1
        fi
    else
        raw_token="$(openssl rand -hex 32 2>/dev/null)" || {
            installer_error "could not generate a client token"
            return 1
        }
        if [[ "${#raw_token}" -ne 64 || ! "$raw_token" =~ ^[[:xdigit:]]{64}$ ]]; then
            installer_error "token generator returned an invalid value"
            return 1
        fi
        client_token_tmp="$config_dir/.client-token.$$"
        if ! printf '%s' "$raw_token" > "$client_token_tmp" \
            || ! chmod 600 "$client_token_tmp" \
            || ! mv -f "$client_token_tmp" "$client_token"; then
            installer_error "could not install client-token"
            return 1
        fi
        client_token_tmp=""
    fi
    if ! valid_header_token "$raw_token"; then
        unset raw_token
        installer_error "client-token must contain only HTTP-header-safe ASCII"
        return 1
    fi
    digest_output="$(printf '%s' "$raw_token" | openssl dgst -sha256 -hex 2>/dev/null)" || {
        unset raw_token
        installer_error "could not calculate the server token verifier"
        return 1
    }
    unset raw_token
    verifier="${digest_output##*= }"
    unset digest_output
    if [[ "${#verifier}" -ne 64 || ! "$verifier" =~ ^[[:xdigit:]]{64}$ ]]; then
        installer_error "SHA-256 utility returned an invalid verifier"
        return 1
    fi
    verifier_tmp="$config_dir/.server-token.sha256.$$"
    if ! printf '%s' "$verifier" > "$verifier_tmp" \
        || ! chmod 600 "$verifier_tmp" \
        || ! mv -f "$verifier_tmp" "$verifier_file"; then
        unset verifier
        installer_error "could not install the server token verifier"
        return 1
    fi
    verifier_tmp=""
    chmod 600 "$client_token" "$verifier_file"

    binary_tmp="$bin_dir/.mcp-shell.$$"
    if ! install -m 755 "$work_dir/build/$native_target/release/mcp-shell" "$binary_tmp" \
        || ! mv -f "$binary_tmp" "$binary_file"; then
        unset verifier
        installer_error "could not install the server binary"
        return 1
    fi
    binary_tmp=""

    escaped_verifier_environment="$(systemd_c_quote "MCP_SHELL_TOKEN_SHA256=$verifier")" || {
        unset verifier
        installer_error "server verifier contains a character that systemd cannot store"
        return 1
    }
    unit_tmp="$unit_dir/.$MCP_SHELL_UNIT.$$"
    if ! {
        printf '[Unit]\nDescription=mcp-shell MCP service\n'
        printf '\n[Service]\nType=exec\n'
        printf 'ExecStart=:/usr/bin/env -- %s --config %s\n' "$escaped_binary" "$escaped_config"
        printf 'Environment=%s %s %s\n' \
            "$escaped_home_environment" "$escaped_path_environment" "$escaped_verifier_environment"
        printf 'UnsetEnvironment=MCP_SHELL_TOKEN\n'
        printf 'Restart=always\nRestartSec=1\n'
        printf 'KillMode=control-group\nTimeoutStopSec=15\n'
        printf 'StandardOutput=journal\nStandardError=journal\n'
        printf '\n[Install]\nWantedBy=default.target\n'
    } > "$unit_tmp"; then
        unset verifier
        installer_error "could not write the systemd user service unit"
        return 1
    fi
    unset verifier
    if ! chmod 600 "$unit_tmp"; then
        installer_error "could not set private service unit permissions"
        return 1
    fi
    if ! mv -f "$unit_tmp" "$unit_file"; then
        installer_error "could not replace the systemd user service unit"
        return 1
    fi
    unit_tmp=""
    if ! systemctl --user daemon-reload; then
        installer_error "could not reload the systemd user manager"
        return 1
    fi
    if ! systemctl --user enable "$MCP_SHELL_UNIT"; then
        installer_error "could not enable the systemd user service"
        return 1
    fi
    if ! systemctl --user restart "$MCP_SHELL_UNIT"; then
        systemctl --user stop "$MCP_SHELL_UNIT" >/dev/null 2>&1 || :
        systemctl --user disable "$MCP_SHELL_UNIT" >/dev/null 2>&1 || :
        installer_error "could not start the systemd user service; check $journal_command"
        return 1
    fi

    started=0
    previous_pid=""
    for ((attempt = 0; attempt < 10; attempt++)); do
        current_pid="$(service_pid "$MCP_SHELL_UNIT" || :)"
        if [[ -n "$current_pid" && "$current_pid" == "$previous_pid" ]]; then
            started=1
            break
        fi
        previous_pid="$current_pid"
        sleep 1
    done
    if [[ "$started" -ne 1 ]]; then
        systemctl --user stop "$MCP_SHELL_UNIT" >/dev/null 2>&1 || :
        systemctl --user disable "$MCP_SHELL_UNIT" >/dev/null 2>&1 || :
        installer_error "systemd user service did not start; check $journal_command"
        return 1
    fi

    printf 'Installed mcp-shell from %s and started user service %s.\n' "$ref" "$MCP_SHELL_UNIT"
    printf '\nInstalled files:\n'
    printf '  %-20s  %s\n' \
        'Executable:' "$binary_file" \
        'Configuration:' "$server_config" \
        'Initialization:' "$init_file" \
        'Client token:' "$client_token" \
        'Server verifier:' "$verifier_file" \
        'User service unit:' "$unit_file" \
        'Logs:' "$journal_command"
)

usage() {
    printf 'Usage: linux-install.sh [--ref REF] [--help]\n'
    printf 'Install mcp-shell for the current Ubuntu user and start its systemd user service.\n'
    printf 'REF can be a branch, tag, commit ID, or full ref; default: main.\n'
}

main() {
    local ref="main"

    while [[ "$#" -gt 0 ]]; do
        case "$1" in
            --help|-h)
                usage
                return 0
                ;;
            --ref)
                if [[ "$#" -lt 2 ]]; then
                    installer_error "--ref requires a value"
                    usage >&2
                    return 2
                fi
                ref="$2"
                shift 2
                ;;
            *)
                installer_error "unknown argument: $1"
                usage >&2
                return 2
                ;;
        esac
    done

    if [[ -z "${HOME:-}" ]]; then
        installer_error "HOME is not set"
        return 1
    fi
    install_linux_for_home "$HOME" "$ref"
}

if [[ "${BASH_SOURCE[0]:-$0}" == "$0" ]]; then
    main "$@"
fi
