#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf 'usage: nomad [--waypipe|-wp] [ssh options] host\n' >&2
}

log() {
  printf 'nomad: %s\n' "$*" >&2
}

resolve_repo_root() {
  local script_path script_dir repo_root

  if [[ -n "${DOTFILES_DIR-}" ]]; then
    repo_root="$DOTFILES_DIR"
  else
    script_path="$(readlink -f "${BASH_SOURCE[0]}")"
    script_dir="$(dirname "$script_path")"
    repo_root="$(cd "$script_dir/../.." && pwd)"
  fi

  if [[ "$repo_root" == "$HOME" ]]; then
    printf 'nomad: refusing to use %s as dotfiles root\n' "$HOME" >&2
    printf 'nomad: set DOTFILES_DIR explicitly if your repo really lives elsewhere\n' >&2
    exit 1
  fi

  if [[ ! -f "$repo_root/.zshrc" || ! -d "$repo_root/.config" || ! -f "$repo_root/profiles/terminal.links" ]]; then
    printf 'nomad: dotfiles root does not look valid: %s\n' "$repo_root" >&2
    printf 'nomad: expected terminal config and profile metadata under %s\n' "$repo_root" >&2
    exit 1
  fi

  if ! command -v git >/dev/null 2>&1 \
    || ! git -C "$repo_root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    printf 'nomad: dotfiles root must be a Git working tree and local git must be installed\n' >&2
    exit 1
  fi

  printf '%s\n' "$repo_root"
}

terminal_payload() {
  local repo_root="$1"
  local manifest="$repo_root/profiles/terminal.links"
  local item tracked

  while IFS= read -r item || [[ -n "$item" ]]; do
    [[ -z "$item" || "$item" == \#* ]] && continue
    tracked="$(git -C "$repo_root" ls-files -- "$item")"
    if [[ -z "$tracked" ]]; then
      printf 'nomad: terminal payload entry is not tracked: %s\n' "$item" >&2
      return 1
    fi
    printf '%s\n' "$tracked"
  done < "$manifest"

  printf '%s\n' \
    .config/nomad/bashrc \
    install-tools.sh \
    update-versions.sh \
    versions.toml \
    profiles/terminal.links
}

session_state_file() {
  local repo_root=$1 ssh_dest=$2 state_dir key_source hash
  shift 2

  state_dir="${XDG_STATE_HOME:-$HOME/.local/state}/nomad"
  mkdir -p "$state_dir"

  key_source="$repo_root"$'\n'"$ssh_dest"
  while [[ $# -gt 0 ]]; do
    key_source+=$'\n'"$1"
    shift
  done

  if command -v sha256sum >/dev/null 2>&1; then
    hash="$(printf '%s' "$key_source" | sha256sum)"
    hash="${hash%% *}"
  elif command -v shasum >/dev/null 2>&1; then
    hash="$(printf '%s' "$key_source" | shasum -a 256)"
    hash="${hash%% *}"
  else
    hash="$(printf '%s' "$key_source" | cksum)"
    hash="${hash// /-}"
  fi

  printf '%s/%s\n' "$state_dir" "$hash"
}

run_remote_shell() {
  local repo_root ssh_dest socket tmp_remote status remote_shell remote_shell_name
  local remote_launcher state_file payload_file
  local use_waypipe=0 payload_needed=0
  local -a ssh_args=()

  repo_root="$(resolve_repo_root)"

  if [[ $# -eq 0 ]]; then
    usage
    exit 1
  fi

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --waypipe|-wp)
        use_waypipe=1
        shift
        ;;
      --)
        printf 'nomad: remote commands are not supported; this always launches your dotfiles shell\n' >&2
        exit 1
        ;;
      -[BbcDEeFIiJlmOopQRSWw])
        if [[ $# -lt 2 ]]; then
          usage
          exit 1
        fi
        ssh_args+=("$1" "$2")
        shift 2
        ;;
      -* )
        ssh_args+=("$1")
        shift
        ;;
      *)
        ssh_dest="$1"
        shift
        if [[ $# -gt 0 ]]; then
          printf 'nomad: remote commands are not supported; this always launches your dotfiles shell\n' >&2
          exit 1
        fi
        break
        ;;
    esac
  done

  if [[ -z "${ssh_dest-}" ]]; then
    usage
    exit 1
  fi

  if [[ $use_waypipe -eq 1 ]] && ! command -v waypipe >/dev/null 2>&1; then
    printf 'nomad: waypipe mode requires local waypipe\n' >&2
    exit 1
  fi

  socket="$(mktemp -u "${TMPDIR:-/tmp}/nomad-socket.XXXXXXXX")"
  payload_file="$(mktemp "${TMPDIR:-/tmp}/nomad-payload.XXXXXXXX")"
  cleanup_local() {
    if [[ -n "${socket-}" && -n "${ssh_dest-}" ]]; then
      ssh -S "$socket" -O exit "${ssh_args[@]}" "$ssh_dest" >/dev/null 2>&1 || true
    fi
    if [[ -n "${socket-}" ]]; then
      rm -f "$socket"
    fi
    if [[ -n "${payload_file-}" ]]; then
      rm -f "$payload_file"
    fi
  }
  trap cleanup_local EXIT

  terminal_payload "$repo_root" > "$payload_file"

  log "using dotfiles repo $repo_root"
  log "opening control connection to $ssh_dest"
  ssh -MNf -o ControlMaster=yes -o ControlPersist=yes -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest"

  state_file="$(session_state_file "$repo_root" "$ssh_dest" "${ssh_args[@]}")"
  tmp_remote=""
  if [[ -f "$state_file" ]]; then
    IFS= read -r tmp_remote < "$state_file" || tmp_remote=""
  fi

  if [[ -n "$tmp_remote" ]] && ssh -S "$socket" -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest" "test -d $(printf '%q' "$tmp_remote") && test -f $(printf '%q' "$tmp_remote/.zshrc")" >/dev/null 2>&1; then
    log "reusing remote temp directory $tmp_remote"
    if ! ssh -S "$socket" -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest" "test -f $(printf '%q' "$tmp_remote/.config/nomad/bashrc")" >/dev/null 2>&1; then
      log 'updating reused remote directory with Bash support'
      payload_needed=1
    fi
  else
    [[ -n "$tmp_remote" ]] && log "saved remote temp directory is gone; creating a new one"
    log 'creating remote temp directory'
    tmp_remote="$(ssh -S "$socket" -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest" 'mktemp -d "${TMPDIR:-/tmp}/nomad.XXXXXXXX"')"
    payload_needed=1
  fi

  if [[ $payload_needed -eq 1 ]]; then
    log "streaming dotfiles to $ssh_dest:$tmp_remote"
    tar -C "$repo_root" -cf - -T "$payload_file" \
      | ssh -S "$socket" -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest" "tar -C $(printf '%q' "$tmp_remote") -xf -"
    printf '%s\n' "$tmp_remote" > "$state_file"
  fi

  log 'resolving remote shell'
  if ! remote_shell="$(ssh -S "$socket" -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest" 'command -v zsh 2>/dev/null || command -v bash 2>/dev/null')" \
    || [[ -z "$remote_shell" ]]; then
    printf 'nomad: neither zsh nor bash is installed on the remote host\n' >&2
    exit 1
  fi
  remote_shell_name="$(basename "$remote_shell")"

  case "$remote_shell_name" in
    zsh|bash) ;;
    *)
      printf 'nomad: unsupported remote shell resolved: %s\n' "$remote_shell" >&2
      exit 1
      ;;
  esac
  log "using remote $remote_shell_name at $remote_shell"

  if [[ $use_waypipe -eq 1 ]]; then
    log 'checking remote waypipe'
    if ! ssh -S "$socket" -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest" 'command -v waypipe >/dev/null 2>&1'; then
      printf 'nomad: waypipe mode requires waypipe on the remote host\n' >&2
      exit 1
    fi
  fi

  log 'writing remote launcher'
  remote_launcher="$tmp_remote/.nomad-shell"
  {
    printf '#!/usr/bin/env sh\n'
    printf 'export SHELL=%s\n' "$(printf '%q' "$remote_shell")"
    printf 'export DOTS=%s\n' "$(printf '%q' "$tmp_remote")"
    printf 'export CODEX_HOME=%s\n' "$(printf '%q' "$tmp_remote/.codex")"
    printf 'mkdir -p "$CODEX_HOME" || exit 1\n'
    printf 'chmod 700 "$CODEX_HOME" || exit 1\n'
    printf 'export NOMAD_BIN=%s\n' "$(printf '%q' "$tmp_remote/.local/bin")"
    printf 'export XDG_BIN_HOME=%s\n' "$(printf '%q' "$tmp_remote/.local/bin")"
    printf 'export XDG_CONFIG_HOME=%s\n' "$(printf '%q' "$tmp_remote/.config")"
    printf 'export XDG_DATA_HOME=%s\n' "$(printf '%q' "$tmp_remote/.local/share")"
    printf 'export XDG_STATE_HOME=%s\n' "$(printf '%q' "$tmp_remote/.local/state")"
    printf 'export XDG_CACHE_HOME=%s\n' "$(printf '%q' "$tmp_remote/.cache")"
    printf 'export PATH=%s:$PATH\n' "$(printf '%q' "$tmp_remote/.local/bin")"
    if [[ "$remote_shell_name" == "zsh" ]]; then
      printf 'export ZDOTDIR=%s\n' "$(printf '%q' "$tmp_remote")"
      printf '%s -il\n' "$(printf '%q' "$remote_shell")"
    else
      printf 'unset ZDOTDIR\n'
      printf '%s --noprofile --rcfile %s -i\n' \
        "$(printf '%q' "$remote_shell")" \
        "$(printf '%q' "$tmp_remote/.config/nomad/bashrc")"
    fi
    printf 'nomad_status=$?\n'
    printf 'exit $nomad_status\n'
  } | ssh -S "$socket" -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest" "cat > $(printf '%q' "$remote_launcher") && chmod +x $(printf '%q' "$remote_launcher")"

  if [[ "$remote_shell_name" == "zsh" ]]; then
    {
      printf '\n# nomad helper begin\n'
      printf 'damon() {\n'
      printf '  local nomad_dir="${DOTS-}"\n'
      printf '  if [[ -z "$nomad_dir" || ! -f "$nomad_dir/.nomad-shell" ]]; then\n'
      printf '    print -u2 "damon: not inside a nomad temp environment"\n'
      printf '    return 1\n'
      printf '  fi\n'
      printf '  cd "${HOME:-/}" 2>/dev/null || cd / 2>/dev/null || true\n'
      printf '  command rm -rf -- "$nomad_dir"\n'
      printf '  exit 0\n'
      printf '}\n'
      printf '# nomad helper end\n'
    } | ssh -S "$socket" -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest" "grep -q '^# nomad helper begin$' $(printf '%q' "$tmp_remote/.zshrc") 2>/dev/null || cat >> $(printf '%q' "$tmp_remote/.zshrc")"
  fi

  status=0

  if [[ $use_waypipe -eq 1 ]]; then
    log "starting remote $remote_shell_name with temporary dotfiles through waypipe"
    waypipe ssh -tt -S "$socket" -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest" "$remote_launcher" || status=$?
  else
    log "starting remote $remote_shell_name with temporary dotfiles"
    ssh -tt -S "$socket" -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest" "$remote_launcher" || status=$?
  fi

  if ssh -S "$socket" -o ControlPath="$socket" "${ssh_args[@]}" "$ssh_dest" "test -d $(printf '%q' "$tmp_remote")" >/dev/null 2>&1; then
    if [[ $status -eq 0 ]]; then
      log 'remote session ended; temporary dotfiles kept'
    else
      log "remote session exited with status $status; temporary dotfiles kept"
    fi
  else
    rm -f "$state_file"
    if [[ $status -eq 0 ]]; then
      log 'remote session ended; temporary dotfiles removed'
    else
      log "remote session exited with status $status; temporary dotfiles removed"
    fi
  fi

  cleanup_local
  trap - EXIT
  return "$status"
}

run_remote_shell "$@"
