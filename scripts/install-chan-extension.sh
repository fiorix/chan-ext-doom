#!/usr/bin/env bash
set -euo pipefail

: "${HOME:?HOME must be set}"

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
runtime_dir="$repo_root/runtime"
install_root=${CHAN_EXT_DOOM_INSTALL_ROOT:-"${HOME}/.local/lib/chan-ext-doom"}
chan_home=${CHAN_HOME:-"${HOME}/.chan"}
binary_path="$install_root/chan-ext-doom"
assets_dir="$install_root/share/chan-ext-doom"
licenses_dir="$install_root/licenses"
config_dir="$chan_home/extensions"
config_path="$config_dir/chan-ext-doom.toml"

cd "$repo_root"
cargo build --locked --release -p doom-extension

install -d "$assets_dir" "$licenses_dir" "$config_dir"
install -m 0755 target/release/chan-ext-doom "$binary_path"
for asset in doom.js doom.wasm doom1.wad; do
    install -m 0644 "$runtime_dir/$asset" "$assets_dir/$asset"
done
install -m 0644 LICENSE-APACHE "$licenses_dir/LICENSE-APACHE"
install -m 0644 engine/COPYING.md "$licenses_dir/engine-GPL-2.0.txt"
install -m 0644 runtime/doom-shareware-license.txt "$licenses_dir/doom-shareware.txt"

toml_command=${binary_path//\\/\\\\}
toml_command=${toml_command//\"/\\\"}
config_tmp=$(mktemp "$config_path.tmp.XXXXXX")
trap 'rm -f -- "$config_tmp"' EXIT
printf 'name = "Doomit"\ncommand = "%s"\nargs = []\ncapabilities = ["session-context", "presentation"]\n' \
    "$toml_command" >"$config_tmp"
chmod 0644 "$config_tmp"
mv -f -- "$config_tmp" "$config_path"
trap - EXIT

printf 'Installed Doomit at %s\nRestart Chan to discover it.\n' "$install_root"
