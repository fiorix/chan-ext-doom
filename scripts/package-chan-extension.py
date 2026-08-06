#!/usr/bin/env python3
"""Assemble a self-contained Chan extension release archive."""

from __future__ import annotations

import argparse
import gzip
import logging
import os
import shutil
import stat
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

LOG = logging.getLogger("chan-ext-doom-package")


@dataclass(frozen=True)
class Target:
    archive: str
    executable: str
    zip: bool = False


TARGETS = {
    "linux-x86_64": Target("chan-ext-doom-linux-x86_64.tar.gz", "chan-ext-doom"),
    "linux-aarch64": Target("chan-ext-doom-linux-aarch64.tar.gz", "chan-ext-doom"),
    "windows-x86_64": Target(
        "chan-ext-doom-windows-x86_64.zip", "chan-ext-doom.exe", zip=True
    ),
    "macos-aarch64": Target("chan-ext-doom-macos-aarch64.tar.gz", "chan-ext-doom"),
}


def workspace_version(repo_root: Path) -> str:
    with (repo_root / "Cargo.toml").open("rb") as cargo_toml:
        document = tomllib.load(cargo_toml)
    return str(document["workspace"]["package"]["version"])


def source_date_epoch(repo_root: Path) -> int:
    configured = os.environ.get("SOURCE_DATE_EPOCH")
    if configured is not None:
        return int(configured)
    result = subprocess.run(
        ["git", "show", "-s", "--format=%ct", "HEAD"],
        cwd=repo_root,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return int(result.stdout.strip())


def copy_file(source: Path, destination: Path, mode: int = 0o644) -> None:
    if not source.is_file():
        raise FileNotFoundError(f"required release input is missing: {source}")
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    destination.chmod(mode)


def build_layout(
    repo_root: Path,
    binary: Path,
    target: Target,
    stage: Path,
    version: str,
    epoch: int,
) -> Path:
    payload = stage / "chan-ext-doom"
    copy_file(binary, payload / target.executable, 0o755)
    copy_file(
        repo_root / "packaging/chan-extension/chan-ext-doom.toml", payload / "chan-ext-doom.toml"
    )
    for name in ("doom.js", "doom.wasm", "doom1.wad"):
        copy_file(repo_root / "runtime" / name, payload / "share/chan-ext-doom" / name)
    copy_file(
        repo_root / "LICENSE-APACHE", payload / "licenses/LICENSE-APACHE"
    )
    copy_file(
        repo_root / "engine/COPYING.md",
        payload / "licenses/engine-GPL-2.0.txt",
    )
    copy_file(
        repo_root / "runtime/doom-shareware-license.txt",
        payload / "licenses/doom-shareware.txt",
    )

    source_archive = payload / "source/doom-engine-source.tar.gz"
    source_archive.parent.mkdir(parents=True, exist_ok=True)
    source_tar = stage / "doom-engine-source.tar"
    subprocess.run(
        [
            "git",
            "archive",
            "--format=tar",
            f"--mtime=@{epoch}",
            f"--prefix=doomit-engine-source-{version}/",
            f"--output={source_tar}",
            "HEAD:engine",
        ],
        cwd=repo_root,
        check=True,
    )
    with source_tar.open("rb") as source, source_archive.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as zipped:
            shutil.copyfileobj(source, zipped)
    source_tar.unlink()
    source_archive.chmod(0o644)
    return payload


def normalized_tar_info(epoch: int):
    def normalize(info: tarfile.TarInfo) -> tarfile.TarInfo:
        info.uid = 0
        info.gid = 0
        info.uname = "root"
        info.gname = "root"
        info.mtime = epoch
        return info

    return normalize


def write_tar(payload: Path, output: Path, epoch: int) -> None:
    with output.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=epoch) as zipped:
            with tarfile.open(fileobj=zipped, mode="w", format=tarfile.PAX_FORMAT) as archive:
                archive.add(
                    payload,
                    arcname="chan-ext-doom",
                    recursive=True,
                    filter=normalized_tar_info(epoch),
                )


def write_zip(payload: Path, output: Path, epoch: int) -> None:
    minimum_zip_epoch = 315_532_800
    timestamp = datetime.fromtimestamp(
        max(epoch, minimum_zip_epoch), tz=timezone.utc
    ).timetuple()[:6]
    with zipfile.ZipFile(
        output, mode="w", compression=zipfile.ZIP_DEFLATED, compresslevel=9
    ) as archive:
        for path in sorted(item for item in payload.rglob("*") if item.is_file()):
            relative = path.relative_to(payload.parent).as_posix()
            info = zipfile.ZipInfo(relative, date_time=timestamp)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.create_system = 3
            mode = stat.S_IFREG | stat.S_IMODE(path.stat().st_mode)
            info.external_attr = mode << 16
            archive.writestr(info, path.read_bytes())


def build_package(repo_root: Path, target_name: str, binary: Path, output_dir: Path) -> Path:
    target = TARGETS[target_name]
    version = workspace_version(repo_root)
    epoch = source_date_epoch(repo_root)
    output_dir.mkdir(parents=True, exist_ok=True)
    output = output_dir / target.archive
    LOG.info("packaging %s from %s", target_name, binary)
    descriptor, pending_name = tempfile.mkstemp(
        prefix=f".{target.archive}.", dir=output_dir
    )
    os.close(descriptor)
    pending = Path(pending_name)
    try:
        with tempfile.TemporaryDirectory(prefix="chan-ext-doom-package-") as temporary:
            payload = build_layout(
                repo_root, binary, target, Path(temporary), version, epoch
            )
            if target.zip:
                write_zip(payload, pending, epoch)
            else:
                write_tar(payload, pending, epoch)
        pending.replace(output)
    finally:
        pending.unlink(missing_ok=True)
    LOG.info("wrote %s", output)
    return output


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=sorted(TARGETS))
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("-v", "--verbose", action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.WARNING,
        format="chan-ext-doom-package: %(message)s",
    )
    repo_root = Path(__file__).resolve().parent.parent
    output = build_package(repo_root, args.target, args.binary.resolve(), args.output_dir)
    print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
