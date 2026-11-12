#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf 'Usage: %s <p12> <password-file> <keychain-path> <identity>\n' "$(basename "$0")" >&2
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

if [ "$#" -ne 4 ]; then
  usage
  exit 2
fi

p12_file=$1
password_file=$2
keychain=$3
identity=$4

[ -n "$p12_file" ] || fail 'p12 path is required'
[ -n "$password_file" ] || fail 'p12 password file is required'
[ -n "$keychain" ] || fail 'keychain path is required'
[ -n "$identity" ] || fail 'signing identity is required'
[ -f "$p12_file" ] || fail "p12 file not found: $p12_file"
[ -f "$password_file" ] || fail "password file not found: $password_file"

[ "$(uname -s)" = 'Darwin' ] || fail 'macOS is required'
command -v security >/dev/null 2>&1 || fail 'security is required'
command -v openssl >/dev/null 2>&1 || fail 'openssl is required'

script_dir=$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(cd -P "$script_dir/.." && pwd -P)

physical_path() {
  local input=$1
  case "$input" in
    /*) ;;
    *) input="$PWD/$input" ;;
  esac

  local resolved='' part candidate
  local -a parts=()
  local IFS='/'
  read -r -a parts <<< "$input"
  for part in "${parts[@]}"; do
    case "$part" in
      ''|.) continue ;;
      ..) resolved=$(dirname "${resolved:-/}") ;;
      *)
        candidate="${resolved:-}/$part"
        if [ -d "$candidate" ]; then
          resolved=$(cd -P "$candidate" && pwd -P)
        elif [ -e "$candidate" ] || [ -L "$candidate" ]; then
          resolved=$(cd -P "${resolved:-/}" && pwd -P)/$part
        else
          resolved="$candidate"
        fi
        ;;
    esac
  done
  printf '%s\n' "${resolved:-/}"
}

keychain_dir=$(dirname "$keychain")
[ -d "$keychain_dir" ] || fail "keychain directory not found: $keychain_dir"
keychain_dir=$(cd -P "$keychain_dir" && pwd -P)
keychain="$keychain_dir/$(basename "$keychain")"

keychain_physical=$(physical_path "$keychain")
case "$keychain_physical" in
  "$repo_root"|"$repo_root"/*)
    fail "keychain path must be outside the repository: $keychain_physical"
    ;;
esac

# Also probe the keychain database suffix ("$keychain-db"), not just the path itself.
if [ -e "$keychain" ] || [ -e "$keychain-db" ]; then
  fail "keychain already exists: $keychain"
fi

password_file_path="$keychain.password"
if [ -e "$password_file_path" ]; then
  fail "keychain password file already exists: $password_file_path"
fi

p12_password=$(cat "$password_file")
[ -n "$p12_password" ] || fail "p12 password file is empty: $password_file"

search_list=()
while IFS= read -r entry; do
  entry=$(printf '%s' "$entry" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/"//g')
  [ -n "$entry" ] || continue
  search_list+=("$entry")
done < <(security list-keychains -d user)

keychain_created=0
search_list_modified=0
completed=0

cleanup_on_failure() {
  local status=$?
  if [ "$completed" -eq 1 ]; then
    exit "$status"
  fi
  trap - EXIT
  if [ "$search_list_modified" -eq 1 ] && [ "${#search_list[@]}" -gt 0 ]; then
    security list-keychains -d user -s "${search_list[@]}" >/dev/null 2>&1 || true
  fi
  if [ "$keychain_created" -eq 1 ]; then
    security delete-keychain "$keychain" >/dev/null 2>&1 || rm -f "$keychain"
  fi
  rm -f "$password_file_path"
  exit "$status"
}
trap cleanup_on_failure EXIT

umask 077
openssl rand -hex 24 > "$password_file_path"
chmod 600 "$password_file_path"
keychain_password=$(cat "$password_file_path")
[ -n "$keychain_password" ] || fail 'generated keychain password is empty'

security create-keychain -p "$keychain_password" "$keychain"
keychain_created=1
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"

security import "$p12_file" -k "$keychain" -P "$p12_password" \
  -T /usr/bin/codesign -T /usr/bin/security >/dev/null

security set-key-partition-list -S apple-tool:,apple:,codesign: \
  -s -k "$keychain_password" "$keychain" >/dev/null

if [ "${#search_list[@]}" -gt 0 ]; then
  security list-keychains -d user -s "$keychain" "${search_list[@]}"
else
  security list-keychains -d user -s "$keychain"
fi
search_list_modified=1

if ! valid_identities=$(security find-identity -v -p codesigning "$keychain" 2>&1); then
  fail "failed to list code-signing identities in $keychain"
fi
case "$valid_identities" in
  *"$identity"*) ;;
  *)
    # Newer macOS omits an untrusted self-signed leaf from the valid-only
    # listing even though codesign can still use its private key.
    if ! listed_identities=$(security find-identity -p codesigning "$keychain" 2>&1); then
      fail "failed to list code-signing identities in $keychain"
    fi
    case "$listed_identities" in
      *"$identity"*)
        printf 'warning: identity is not valid-trusted; the valid-only listing omits it\n' >&2
        ;;
      *) fail "identity not found after import: $identity" ;;
    esac
    ;;
esac

completed=1
printf 'Keychain: %s\n' "$keychain"
printf 'Keychain password file: %s\n' "$password_file_path"
printf 'Identity: %s\n' "$identity"
