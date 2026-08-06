#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/chan-ext-doom-install-test.XXXXXX")
trap 'rm -rf -- "$temporary_dir"' EXIT

release_dir="$temporary_dir/release"
binary="$temporary_dir/chan-ext-doom"
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
    sha256sum chan-ext-doom-* >SHA256SUMS
)

# shellcheck disable=SC2016  # The generated helper expands these variables.
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'case "${1:-}" in' \
    '    -s) printf "%s\n" "${CHAN_EXT_DOOM_TEST_SYSTEM:?}" ;;' \
    '    -m) printf "%s\n" "${CHAN_EXT_DOOM_TEST_MACHINE:?}" ;;' \
    '    *) exit 2 ;;' \
    'esac' >"$fake_bin/uname"
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'printf "%s\n" "C:\\chan-ext-doom\\chan-ext-doom.exe"' >"$fake_bin/cygpath"
chmod 0755 "$fake_bin/uname" "$fake_bin/cygpath"

run_install() {
    local label=$1
    local system=$2
    local machine=$3
    local executable=$4
    local install_root="$temporary_dir/$label/.local/lib/chan-ext-doom"
    local chan_home="$temporary_dir/$label/.chan"

    PATH="$fake_bin:$PATH" \
        CHAN_EXT_DOOM_TEST_SYSTEM="$system" \
        CHAN_EXT_DOOM_TEST_MACHINE="$machine" \
        CHAN_EXT_DOOM_RELEASE_BASE_URL="file://$release_dir" \
        CHAN_EXT_DOOM_INSTALL_ROOT="$install_root" \
        CHAN_HOME="$chan_home" \
        "$repo_root/install.sh" >/dev/null

    cmp "$binary" "$install_root/$executable"
    [[ -x "$install_root/$executable" ]]
    [[ -f "$install_root/share/chan-ext-doom/doom.wasm" ]]
    [[ -f "$install_root/licenses/doom-shareware.txt" ]]
    [[ -f "$install_root/source/doom-engine-source.tar.gz" ]]
    grep -Fq 'name = "Doomit"' "$chan_home/extensions/chan-ext-doom.toml"
}

run_install linux-x86_64 Linux x86_64 chan-ext-doom
run_install linux-aarch64 Linux aarch64 chan-ext-doom
run_install macos-aarch64 Darwin arm64 chan-ext-doom
run_install windows-x86_64 MINGW64_NT-10.0 x86_64 chan-ext-doom.exe

tar -tzf "$temporary_dir/linux-x86_64/.local/lib/chan-ext-doom/source/doom-engine-source.tar.gz" >"$temporary_dir/source-files"
grep -q '/COPYING.md$' "$temporary_dir/source-files"
grep -Fq \
    "command = \"$temporary_dir/linux-x86_64/.local/lib/chan-ext-doom/chan-ext-doom\"" \
    "$temporary_dir/linux-x86_64/.chan/extensions/chan-ext-doom.toml"
grep -Fq \
    'command = "C:\\chan-ext-doom\\chan-ext-doom.exe"' \
    "$temporary_dir/windows-x86_64/.chan/extensions/chan-ext-doom.toml"

printf 'corrupt' >>"$release_dir/chan-ext-doom-linux-x86_64.tar.gz"
if PATH="$fake_bin:$PATH" \
    CHAN_EXT_DOOM_TEST_SYSTEM=Linux \
    CHAN_EXT_DOOM_TEST_MACHINE=x86_64 \
    CHAN_EXT_DOOM_RELEASE_BASE_URL="file://$release_dir" \
    CHAN_EXT_DOOM_INSTALL_ROOT="$temporary_dir/bad-install" \
    CHAN_HOME="$temporary_dir/bad-chan" \
    "$repo_root/install.sh" >"$temporary_dir/bad.stdout" 2>"$temporary_dir/bad.stderr"; then
    printf 'corrupt archive unexpectedly installed\n' >&2
    exit 1
fi
grep -Fq 'checksum mismatch' "$temporary_dir/bad.stderr"
[[ ! -e "$temporary_dir/bad-install/chan-ext-doom" ]]
