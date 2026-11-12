#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf 'Usage: %s <keychain-path> <identity> <certificate-output-dir>\n' "$(basename "$0")" >&2
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

if [ "$#" -ne 3 ]; then
  usage
  exit 2
fi

keychain=$1
identity=$2
certificate_dir=$3

[ -n "$keychain" ] || fail 'keychain path is required'
[ -n "$identity" ] || fail 'signing identity is required'
[ -n "$certificate_dir" ] || fail 'certificate output directory is required'
[ -e "$keychain" ] || fail "keychain not found: $keychain"
[ -d "$certificate_dir" ] || fail "certificate output directory not found: $certificate_dir"

certificate_files=(
  certificate.pem
  certificate-key.pem
  certificate.p12
  certificate-password.txt
  openssl.cnf
)
for name in "${certificate_files[@]}"; do
  [ -f "$certificate_dir/$name" ] || fail "missing certificate file: $certificate_dir/$name"
done

[ "$(uname -s)" = 'Darwin' ] || fail 'macOS is required'
for tool in clang codesign security; do
  command -v "$tool" >/dev/null 2>&1 || fail "$tool is required"
done

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

keychain=$(physical_path "$keychain")
certificate_dir=$(physical_path "$certificate_dir")
case "$keychain" in
  "$repo_root"|"$repo_root"/*) fail "keychain path must be outside the repository: $keychain" ;;
esac
case "$certificate_dir" in
  "$repo_root"|"$repo_root"/*) fail "certificate output must be outside the repository: $certificate_dir" ;;
esac

keychain_password_file="$keychain.password"
workdir=$(mktemp -d "${TMPDIR:-/tmp}/oneget-signcheck.XXXXXX")
workdir=$(cd -P "$workdir" && pwd -P)
case "$workdir" in
  "$repo_root"|"$repo_root"/*)
    rm -rf "$workdir"
    fail "temporary directory must be outside the repository: $workdir"
    ;;
esac

probe_source="$workdir/signcheck.c"
probe_binary="$workdir/signcheck"
identifier='com.oneget.signcheck'

cleanup() {
  local status=$?
  trap - EXIT

  if [ -n "${keychain:-}" ]; then
    local remaining=() entry
    while IFS= read -r entry; do
      entry=$(printf '%s' "$entry" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/"//g')
      [ -n "$entry" ] || continue
      [ "$entry" = "$keychain" ] && continue
      remaining+=("$entry")
    done < <(security list-keychains -d user 2>/dev/null || true)
    if [ "${#remaining[@]}" -gt 0 ]; then
      security list-keychains -d user -s "${remaining[@]}" >/dev/null 2>&1 || true
    fi
    security delete-keychain "$keychain" >/dev/null 2>&1 || rm -f "$keychain"
  fi

  rm -f "$keychain_password_file"
  [ -n "${workdir:-}" ] && rm -rf "$workdir"
  for name in "${certificate_files[@]}"; do
    rm -f "${certificate_dir:-}/$name"
  done
  [ -n "${certificate_dir:-}" ] && rmdir "$certificate_dir" 2>/dev/null || true
  exit "$status"
}
trap cleanup EXIT

if [ -f "$keychain_password_file" ]; then
  keychain_password=$(cat "$keychain_password_file")
  if [ -n "$keychain_password" ]; then
    security unlock-keychain -p "$keychain_password" "$keychain" >/dev/null 2>&1 || true
  fi
fi

cat > "$probe_source" <<'EOF'
int main(void) {
  return 0;
}
EOF

clang -o "$probe_binary" "$probe_source"

codesign --force --sign "$identity" --keychain "$keychain" \
  --identifier "$identifier" --timestamp=none "$probe_binary" >/dev/null

codesign --verify --strict --verbose=2 "$probe_binary" >/dev/null 2>&1 \
  || fail 'codesign --verify --strict failed'

if ! details=$(codesign -dv --verbose=4 "$probe_binary" 2>&1); then
  fail 'codesign -dv failed'
fi

case "$details" in
  *"Identifier=$identifier"*) ;;
  *) fail "unexpected signature identifier, expected $identifier" ;;
esac
case "$details" in
  *"Authority=$identity"*) ;;
  *) fail "signature authority does not match identity: $identity" ;;
esac
case "$details" in
  *"Signature=adhoc"*) fail 'signature is ad-hoc' ;;
esac
case "$details" in
  *"flags="*adhoc*) fail 'signature flags are ad-hoc' ;;
esac

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
      *) fail "identity is no longer discoverable: $identity" ;;
    esac
    ;;
esac

printf 'Verified probe signature: %s\n' "$probe_binary"
printf 'Identifier: %s\n' "$identifier"
printf 'Authority: %s\n' "$identity"
