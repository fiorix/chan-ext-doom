from __future__ import annotations

import importlib.util
import sys
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = REPO_ROOT / "scripts/package-chan-extension.py"
SPEC = importlib.util.spec_from_file_location("package_chan_extension", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
PACKAGE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PACKAGE
SPEC.loader.exec_module(PACKAGE)


class PackageChanExtensionTests(unittest.TestCase):
    def build(self, target: str) -> Path:
        temporary = self.enterContext(tempfile.TemporaryDirectory())
        root = Path(temporary)
        binary = root / PACKAGE.TARGETS[target].executable
        binary.write_bytes(b"test binary")
        return PACKAGE.build_package(REPO_ROOT, target, binary, root / "dist")

    def test_unix_archive_has_the_complete_runtime_layout(self) -> None:
        archive = self.build("linux-x86_64")
        with tarfile.open(archive, "r:gz") as bundle:
            names = set(bundle.getnames())
            executable = bundle.getmember("chan-ext-doom/chan-ext-doom")
            self.assertEqual(executable.mode, 0o755)
        self.assertIn("chan-ext-doom/share/chan-ext-doom/doom.wasm", names)
        self.assertIn("chan-ext-doom/licenses/doom-shareware.txt", names)
        self.assertIn("chan-ext-doom/source/doom-engine-source.tar.gz", names)

    def test_windows_archive_uses_the_exe_name(self) -> None:
        archive = self.build("windows-x86_64")
        with zipfile.ZipFile(archive) as bundle:
            names = set(bundle.namelist())
        self.assertIn("chan-ext-doom/chan-ext-doom.exe", names)
        self.assertIn("chan-ext-doom/share/chan-ext-doom/doom1.wad", names)
        self.assertIn("chan-ext-doom/licenses/engine-GPL-2.0.txt", names)

    def test_archive_is_reproducible(self) -> None:
        first = self.build("linux-x86_64").read_bytes()
        second = self.build("linux-x86_64").read_bytes()
        self.assertEqual(first, second)


if __name__ == "__main__":
    unittest.main()
