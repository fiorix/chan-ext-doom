#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/doomit-install-test.XXXXXX")
trap 'rm -rf -- "$temporary_dir"' EXIT

release_dir="$temporary_dir/release"
binary="$temporary_dir/doomit-extension"
fake_bin="$temporary_dir/bin"
mkdir -p "$release_dir" "$fake_bin"
printf '#!/usr/bin/env sh\nprintf "test binary\\n"\n' >"$binary"
chmod 0755 "$binary"

for target in linux-x86_64 linux-aarch64 windows-x86_64 macos-aarch64; do
    python3 "$repo_root/scripts/package-chan-extension.py" \
        --target "$target" \
        --binary "$binary" \
        --output-dir "$release_dir" >/dev/null
done
(
    cd "$release_dir"
    sha256sum doomit-* >SHA256SUMS
)

# shellcheck disable=SC2016  # The generated helper expands these variables.
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'case "${1:-}" in' \
    '    -s) printf "%s\n" "${DOOMIT_TEST_SYSTEM:?}" ;;' \
    '    -m) printf "%s\n" "${DOOMIT_TEST_MACHINE:?}" ;;' \
    '    *) exit 2 ;;' \
    'esac' >"$fake_bin/uname"
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'printf "%s\n" "C:\\doomit\\doomit-extension.exe"' >"$fake_bin/cygpath"
chmod 0755 "$fake_bin/uname" "$fake_bin/cygpath"

run_install() {
    local label=$1
    local system=$2
    local machine=$3
    local executable=$4
    local install_root="$temporary_dir/$label/.local/lib/doomit"
    local chan_home="$temporary_dir/$label/.chan"

    PATH="$fake_bin:$PATH" \
        DOOMIT_TEST_SYSTEM="$system" \
        DOOMIT_TEST_MACHINE="$machine" \
        DOOMIT_RELEASE_BASE_URL="file://$release_dir" \
        DOOMIT_INSTALL_ROOT="$install_root" \
        CHAN_HOME="$chan_home" \
        "$repo_root/install.sh" >/dev/null

    cmp "$binary" "$install_root/$executable"
    [[ -x "$install_root/$executable" ]]
    [[ -f "$install_root/share/doomit/doom.wasm" ]]
    [[ -f "$install_root/licenses/doom-shareware.txt" ]]
    [[ -f "$install_root/source/doom-engine-source.tar.gz" ]]
    grep -Fq 'name = "Doomit"' "$chan_home/extensions/doomit.toml"
}

run_install linux-x86_64 Linux x86_64 doomit-extension
run_install linux-aarch64 Linux aarch64 doomit-extension
run_install macos-aarch64 Darwin arm64 doomit-extension
run_install windows-x86_64 MINGW64_NT-10.0 x86_64 doomit-extension.exe

tar -tzf "$temporary_dir/linux-x86_64/.local/lib/doomit/source/doom-engine-source.tar.gz" >"$temporary_dir/source-files"
grep -q '/COPYING.md$' "$temporary_dir/source-files"
grep -Fq \
    "command = \"$temporary_dir/linux-x86_64/.local/lib/doomit/doomit-extension\"" \
    "$temporary_dir/linux-x86_64/.chan/extensions/doomit.toml"
grep -Fq \
    'command = "C:\\doomit\\doomit-extension.exe"' \
    "$temporary_dir/windows-x86_64/.chan/extensions/doomit.toml"

printf 'corrupt' >>"$release_dir/doomit-linux-x86_64.tar.gz"
if PATH="$fake_bin:$PATH" \
    DOOMIT_TEST_SYSTEM=Linux \
    DOOMIT_TEST_MACHINE=x86_64 \
    DOOMIT_RELEASE_BASE_URL="file://$release_dir" \
    DOOMIT_INSTALL_ROOT="$temporary_dir/bad-install" \
    CHAN_HOME="$temporary_dir/bad-chan" \
    "$repo_root/install.sh" >"$temporary_dir/bad.stdout" 2>"$temporary_dir/bad.stderr"; then
    printf 'corrupt archive unexpectedly installed\n' >&2
    exit 1
fi
grep -Fq 'checksum mismatch' "$temporary_dir/bad.stderr"
[[ ! -e "$temporary_dir/bad-install/doomit-extension" ]]
