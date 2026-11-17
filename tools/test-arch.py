#!/usr/bin/env python3
"""Exercise source packaging and the lock wrapper without Arch, root or a desktop."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent


class ArchToolsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="amber-arch-test-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / "checkout"
        for directory in (".cargo", "crates", "server", "src", "src-tauri", "tools", "packaging/arch"):
            (self.root / directory).mkdir(parents=True)
        for name in ("Cargo.toml", "Cargo.lock", "package-lock.json", "LICENSE", "README.md",
                     "tsconfig.json", "vite.config.ts", "settings.html", "bubble.html"):
            (self.root / name).write_text("fixture\n")
        (self.root / "package.json").write_text(json.dumps({"version": "0.4.8-preview.1"}))
        shutil.copyfile(ROOT / "tools/arch.sh", self.root / "tools/arch.sh")
        shutil.copyfile(ROOT / "packaging/arch/PKGBUILD.in", self.root / "packaging/arch/PKGBUILD.in")
        self.env = dict(os.environ, SOURCE_DATE_EPOCH="1234567890")

    def prepare(self, destination, expected=0):
        result = subprocess.run(["bash", str(self.root / "tools/arch.sh"), "--prepare-only", str(destination)],
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return result

    def test_snapshot_uses_current_source_and_verified_recipe(self):
        (self.root / "src/current.rs").write_text("uncommitted source\n")
        destination = self.base / "prepared"
        self.prepare(destination)
        archive = destination / "amber-dictation-0.4.8_preview.1.tar.gz"
        with tarfile.open(archive) as contents:
            self.assertEqual(contents.extractfile("amber-dictation-0.4.8_preview.1/src/current.rs").read(), b"uncommitted source\n")
            self.assertTrue(all(member.uid == member.gid == 0 and member.mtime == 1234567890 for member in contents))
        recipe = (destination / "PKGBUILD").read_text()
        self.assertIn(hashlib.sha256(archive.read_bytes()).hexdigest(), recipe)
        self.assertNotIn("@VERSION@", recipe)
        self.assertNotIn("@SHA256@", recipe)
        self.assertNotIn("github.com/EinFabo", recipe)
        subprocess.run(["bash", "-n", str(destination / "PKGBUILD")], check=True)

    def test_snapshot_excludes_generated_files_and_signing_material(self):
        for path in ("src/.env", "src/.env.local", "src/private.key", "tools/signing.pfx",
                     "crates/native/target/a.o", "server/node_modules/private.js", "src-tauri/gen/generated.json"):
            file = self.root / path
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text("must not ship")
        destination = self.base / "prepared"
        self.prepare(destination)
        with tarfile.open(next(destination.glob("*.tar.gz"))) as contents:
            for member in contents:
                if member.isfile():
                    self.assertNotEqual(contents.extractfile(member).read(), b"must not ship", member.name)

    def test_snapshots_are_repeatable_and_preserve_relative_links(self):
        (self.root / "src/file.rs").write_text("source\n")
        (self.root / "src/link.rs").symlink_to("file.rs")
        first, second = self.base / "one", self.base / "two"
        self.prepare(first)
        self.prepare(second)
        self.assertEqual(next(first.glob("*.tar.gz")).read_bytes(), next(second.glob("*.tar.gz")).read_bytes())
        with tarfile.open(next(first.glob("*.tar.gz"))) as contents:
            link = contents.getmember("amber-dictation-0.4.8_preview.1/src/link.rs")
            self.assertTrue(link.issym())
            self.assertEqual(link.linkname, "file.rs")

    def test_existing_or_in_checkout_destinations_are_not_overwritten(self):
        destination = self.base / "occupied"
        destination.mkdir()
        (destination / "keep").write_text("keep")
        self.prepare(destination, 2)
        self.assertEqual((destination / "keep").read_text(), "keep")
        self.prepare(self.root / "staging", 2)
        self.assertFalse((self.root / "staging").exists())

    def test_locker_runs_even_when_amber_cancellation_fails(self):
        commands = self.base / "bin"
        commands.mkdir()
        for name, body in {
            "i3lock": 'printf "locked"; printf " <%s>" "$@"; printf "\\n"',
            "amber-dictation": "exit 1",
            "timeout": "exit 124",
        }.items():
            path = commands / name
            path.write_text("#!/bin/sh\n" + body + "\n")
            path.chmod(0o755)
        result = subprocess.run(["bash", str(ROOT / "tools/amber-i3-lock"), "--nofork", "-c", "000000"],
                                env=dict(os.environ, PATH=f"{commands}:{os.environ['PATH']}"), text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "locked <--nofork> <-c> <000000>\n")


if __name__ == "__main__":
    unittest.main()
