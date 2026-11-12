#!/usr/bin/env bash
set -euo pipefail

usage() {
  printf 'Usage: %s <output-dir> <identity>\n' "$(basename "$0")" >&2
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

if [ "$#" -ne 2 ]; then
  usage
  exit 2
fi

output_dir=$1
identity=$2

[ -n "$output_dir" ] || fail 'output directory is required'
[ -n "$identity" ] || fail 'signing identity is required'
case "$identity" in
  *$'\n'*) fail 'signing identity must not contain a newline' ;;
esac

[ "$(uname -s)" = 'Darwin' ] || fail 'macOS is required'
command -v openssl >/dev/null 2>&1 || fail 'openssl is required'

script_dir=$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
repo_root=$(cd -P "$script_dir/.." && pwd -P)

# Resolve the physical path of a possibly nonexistent target so repository
# containment checks cannot be bypassed with symlinks or relative segments.
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

output_physical=$(physical_path "$output_dir")
case "$output_physical" in
  "$repo_root"|"$repo_root"/*)
    fail "output directory must be outside the repository: $output_physical"
    ;;
esac

umask 077
mkdir -p "$output_dir"
output_dir=$(cd -P "$output_dir" && pwd -P)

config_file="$output_dir/openssl.cnf"
key_file="$output_dir/certificate-key.pem"
cert_file="$output_dir/certificate.pem"
p12_file="$output_dir/certificate.p12"
password_file="$output_dir/certificate-password.txt"

openssl rand -hex 32 > "$password_file"
chmod 600 "$password_file"

cat > "$config_file" <<EOF
[ req ]
distinguished_name = dn
prompt = no
x509_extensions = v3_codesign

[ dn ]
CN = $identity

[ v3_codesign ]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature,keyEncipherment
extendedKeyUsage = critical,codeSigning
subjectKeyIdentifier = hash
EOF
chmod 600 "$config_file"

openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
  -keyout "$key_file" -out "$cert_file" \
  -config "$config_file" -extensions v3_codesign >/dev/null 2>&1

chmod 600 "$key_file" "$cert_file"

p12_help=$(openssl pkcs12 -export -help 2>&1 || true)
if [ -n "$(printf '%s' "$p12_help" | grep -F -- '-keypbe')" ]; then
  openssl pkcs12 -export -in "$cert_file" -inkey "$key_file" -name "$identity" \
    -out "$p12_file" -passout "file:$password_file" \
    -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1 >/dev/null 2>&1
elif [ -n "$(printf '%s' "$p12_help" | grep -F -- '-legacy')" ]; then
  openssl pkcs12 -export -in "$cert_file" -inkey "$key_file" -name "$identity" \
    -out "$p12_file" -passout "file:$password_file" -legacy >/dev/null 2>&1
else
  openssl pkcs12 -export -in "$cert_file" -inkey "$key_file" -name "$identity" \
    -out "$p12_file" -passout "file:$password_file" >/dev/null 2>&1
fi
chmod 600 "$p12_file"

printf 'Certificate: %s\n' "$cert_file"
printf 'Private key: %s\n' "$key_file"
printf 'PKCS#12 bundle: %s\n' "$p12_file"
printf 'Bundle password file: %s\n' "$password_file"
printf 'Next: "%s" "%s" "%s" "<temporary-keychain-path>" "%s"\n' \
  "$script_dir/import-signing-certificate.sh" "$p12_file" "$password_file" "$identity"
