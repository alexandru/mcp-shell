#!/bin/bash

MCP_SHELL_REPOSITORY="https://github.com/alexandru/mcp-shell.git"
MCP_SHELL_LABEL="io.github.alexandru.mcp-shell"

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

xml_escape() {
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
            '&') escaped="${escaped}&amp;" ;;
            '<') escaped="${escaped}&lt;" ;;
            '>') escaped="${escaped}&gt;" ;;
            '"') escaped="${escaped}&quot;" ;;
            "'") escaped="${escaped}&apos;" ;;
            *) escaped="${escaped}${character}" ;;
        esac
    done
    printf '%s' "$escaped"
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
    local pid

    status="$(launchctl print "$target" 2>/dev/null)" || return 1
    while IFS= read -r line; do
        case "$line" in
            *"pid = "*)
                pid=${line##*pid = }
                case "$pid" in
                    ''|*[!0-9]*) continue ;;
                esac
                if ((pid > 0)); then
                    printf '%s' "$pid"
                    return 0
                fi
                ;;
        esac
    done <<EOF
$status
EOF
    return 1
}

install_macos_for_home() (
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
    plist_tmp=""

    cleanup_installer() {
        if [[ -n "$work_dir" ]]; then
            rm -rf "$work_dir" || :
        fi
        for temporary_file in \
            "$server_config_tmp" "$init_tmp" "$client_token_tmp" \
            "$verifier_tmp" "$binary_tmp" "$plist_tmp"; do
            if [[ -n "$temporary_file" ]]; then
                rm -f "$temporary_file" || :
            fi
        done
    }

    trap cleanup_installer EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM

    if [[ "$(uname -s)" != "Darwin" ]]; then
        installer_error "this installer requires macOS"
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
    for tool in git cargo rustc openssl plutil launchctl mktemp install mkdir chmod mv rm sleep; do
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
    xml_home="$(xml_escape "$home_dir")" || {
        installer_error "home path contains a character that XML cannot store"
        return 1
    }
    path_value="${PATH:-/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin}"
    xml_path="$(xml_escape "$path_value")" || {
        installer_error "PATH contains a character that XML cannot store"
        return 1
    }

    launch_domain="gui/$uid"
    if ! launchctl print "$launch_domain" >/dev/null 2>&1; then
        installer_error "no logged-in GUI launchctl domain is available for this user"
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

    config_dir="$home_dir/.config/mcp-shell"
    bin_dir="$home_dir/.local/bin"
    log_dir="$home_dir/Library/Logs/mcp-shell"
    agent_dir="$home_dir/Library/LaunchAgents"
    server_config="$config_dir/server.toml"
    init_file="$config_dir/init.bash"
    client_token="$config_dir/client-token"
    verifier_file="$config_dir/server-token.sha256"
    binary_file="$bin_dir/mcp-shell"
    plist_file="$agent_dir/$MCP_SHELL_LABEL.plist"
    stdout_log="$log_dir/stdout.log"
    stderr_log="$log_dir/stderr.log"

    if ! mkdir -p "$config_dir" "$bin_dir" "$log_dir" "$agent_dir"; then
        installer_error "could not create installation directories"
        return 1
    fi
    if ! chmod 700 "$config_dir" "$log_dir"; then
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
            printf 'bind = "0.0.0.0:8090"\n'
            printf 'allowed_hosts = ["127.0.0.1:8090", "localhost:8090", "host.docker.internal:8090"]\n'
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

    escaped_binary="$(xml_escape "$binary_file")" || {
        unset verifier
        installer_error "binary path contains a character that XML cannot store"
        return 1
    }
    escaped_config="$(xml_escape "$server_config")" || {
        unset verifier
        installer_error "configuration path contains a character that XML cannot store"
        return 1
    }
    escaped_stdout="$(xml_escape "$stdout_log")" || {
        unset verifier
        installer_error "stdout log path contains a character that XML cannot store"
        return 1
    }
    escaped_stderr="$(xml_escape "$stderr_log")" || {
        unset verifier
        installer_error "stderr log path contains a character that XML cannot store"
        return 1
    }
    plist_tmp="$agent_dir/.$MCP_SHELL_LABEL.plist.$$"
    if ! {
        printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>'
        printf '%s\n' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
        printf '%s\n' '<plist version="1.0">' '<dict>'
        printf '%s\n' '  <key>Label</key>' "  <string>$MCP_SHELL_LABEL</string>"
        printf '%s\n' '  <key>ProgramArguments</key>' '  <array>'
        printf '%s\n' "    <string>$escaped_binary</string>" '    <string>--config</string>' "    <string>$escaped_config</string>" '  </array>'
        printf '%s\n' '  <key>EnvironmentVariables</key>' '  <dict>'
        printf '%s\n' '    <key>HOME</key>' "    <string>$xml_home</string>"
        printf '%s\n' '    <key>PATH</key>' "    <string>$xml_path</string>"
        printf '%s\n' '    <key>MCP_SHELL_TOKEN_SHA256</key>' "    <string>$verifier</string>"
        printf '%s\n' '  </dict>' '  <key>RunAtLoad</key>' '  <true/>' '  <key>KeepAlive</key>' '  <true/>'
        printf '%s\n' '  <key>StandardOutPath</key>' "  <string>$escaped_stdout</string>"
        printf '%s\n' '  <key>StandardErrorPath</key>' "  <string>$escaped_stderr</string>"
        printf '%s\n' '</dict>' '</plist>'
    } > "$plist_tmp"; then
        unset verifier
        installer_error "could not write the LaunchAgent plist"
        return 1
    fi
    unset verifier
    if ! chmod 600 "$plist_tmp" || ! plutil -lint "$plist_tmp" >/dev/null 2>&1; then
        installer_error "generated LaunchAgent plist failed validation"
        return 1
    fi

    service_target="$launch_domain/$MCP_SHELL_LABEL"
    if launchctl print "$service_target" >/dev/null 2>&1; then
        if ! launchctl bootout "$service_target" >/dev/null 2>&1; then
            installer_error "could not stop the existing LaunchAgent"
            return 1
        fi
    fi
    if ! mv -f "$plist_tmp" "$plist_file"; then
        installer_error "could not replace the LaunchAgent plist"
        return 1
    fi
    plist_tmp=""
    if ! launchctl enable "$service_target" >/dev/null 2>&1; then
        installer_error "could not enable the LaunchAgent"
        return 1
    fi
    if ! launchctl bootstrap "$launch_domain" "$plist_file" >/dev/null 2>&1; then
        launchctl bootout "$service_target" >/dev/null 2>&1 || :
        installer_error "could not start the LaunchAgent; check $stderr_log"
        return 1
    fi

    started=0
    previous_pid=""
    for attempt in 1 2 3 4 5 6 7 8 9 10; do
        current_pid="$(service_pid "$service_target" || :)"
        if [[ -n "$current_pid" && "$current_pid" == "$previous_pid" ]]; then
            started=1
            break
        fi
        previous_pid="$current_pid"
        sleep 1
    done
    if [[ "$started" -ne 1 ]]; then
        launchctl bootout "$service_target" >/dev/null 2>&1 || :
        installer_error "LaunchAgent did not start; check $stderr_log"
        return 1
    fi

    printf 'Installed mcp-shell from %s and started %s.\n' "$ref" "$service_target"
    printf '\nInstalled files:\n'
    printf '  %-16s  %s\n' \
        'Executable:' "$binary_file" \
        'Configuration:' "$server_config" \
        'Initialization:' "$init_file" \
        'Client token:' "$client_token" \
        'Server verifier:' "$verifier_file" \
        'LaunchAgent:' "$plist_file" \
        'Standard output:' "$stdout_log" \
        'Standard error:' "$stderr_log"
)

usage() {
    printf 'Usage: install-macos.sh [--ref REF] [--help]\n'
    printf 'Install mcp-shell for the current macOS user and start its LaunchAgent.\n'
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
    install_macos_for_home "$HOME" "$ref"
}

if [[ "${BASH_SOURCE[0]:-$0}" == "$0" ]]; then
    main "$@"
fi
