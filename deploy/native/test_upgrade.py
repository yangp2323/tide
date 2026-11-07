"""Exercise release activation/rollback without network access or real services."""
import hashlib
import io
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import unittest


# upgrade.sh runs on the Linux server and relies on GNU tools (flock, mv -T, sha256sum -c).
@unittest.skipUnless(sys.platform.startswith("linux"), "upgrade.sh 只在 Linux 上测试（需要 flock 和 GNU 工具）")
class UpgradeTests(unittest.TestCase):
    def run_upgrade(self, fail_new=False, corrupt=False):
        with tempfile.TemporaryDirectory(prefix="tide-deploy-test-") as temporary:
            root = Path(temporary)
            app = root / "opt/tide"
            old = app / "releases/old"
            old.mkdir(parents=True)
            (old / "tide").write_text("old")
            (old / "tide").chmod(0o755)
            (app / "current").symlink_to(old)
            (app / "data").mkdir()
            for name in ("config.json", "auth.json"):
                (app / "data" / name).write_text("{}")
            with sqlite3.connect(app / "data/tide.sqlite") as connection:
                connection.execute("create table trades(id integer)")
                connection.execute("insert into trades values(42)")
            (root / "etc/tide").mkdir(parents=True)
            (root / "etc/tide/tide.env").write_text("TIDE_PUBLIC_ORIGIN=https://example.invalid\n")
            (root / "run").mkdir()
            (root / "root").mkdir()
            archive = root / "root/tide-new.tgz"
            files = {"tide": b"new", "web/index.html": b"new page"}
            manifest = "".join(f"{hashlib.sha256(data).hexdigest()}  ./{name}\n" for name, data in files.items())
            files["SHA256SUMS"] = manifest.encode()
            with tarfile.open(archive, "w:gz") as package:
                for name, data in files.items():
                    entry = tarfile.TarInfo(name)
                    entry.size = len(data)
                    entry.mode = 0o755 if name == "tide" else 0o644
                    package.addfile(entry, io.BytesIO(data))
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            if corrupt:
                checksum = "0" * 64
            binary = root / "bin"
            binary.mkdir()
            mock = '''#!/usr/bin/python3
import os, pathlib, sqlite3, subprocess, sys
root = pathlib.Path(os.environ["TEST_ROOT"])
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
if name == "id": print(0)
elif name == "systemctl":
    if args[0] == "show": print(123)
    if args[0] == "restart":
        with (root / "restarts").open("a") as log: log.write(str((root / "opt/tide/current").resolve()) + "\\n")
elif name == "readlink":
    if args[-1] == "/proc/123/exe": print((root / "opt/tide/current").resolve() / "tide")
    else: sys.exit(subprocess.call(["/usr/bin/readlink"] + args))
elif name == "sqlite3":
    with sqlite3.connect(args[0]) as db:
        for command in args[1:]:
            if command.startswith(".backup "):
                with sqlite3.connect(command.split("'", 2)[1]) as backup: db.backup(backup)
            elif not command.startswith("."): print(db.execute(command).fetchone()[0])
elif name == "curl":
    if os.environ["FAIL_NEW"] == "1" and (root / "opt/tide/current").resolve().name == "new": sys.exit(22)
'''
            for name in ("id", "systemctl", "readlink", "sqlite3", "curl", "sleep"):
                path = binary / name
                path.write_text(mock)
                path.chmod(0o755)
            script = Path(__file__).with_name("upgrade.sh").read_text()
            for prefix in ("/opt/tide", "/etc/tide", "/root/tide-", "/run/tide-"):
                script = script.replace(prefix, str(root) + prefix)
            environment = dict(os.environ, PATH=f"{binary}:{os.environ['PATH']}", TEST_ROOT=str(root), FAIL_NEW=str(int(fail_new)))
            result = subprocess.run(["bash", "-s", "--", "new", checksum], input=script, text=True, env=environment, capture_output=True)
            expected = old if fail_new or corrupt else app / "releases/new"
            self.assertEqual((app / "current").resolve(), expected, result.stdout + result.stderr)
            self.assertEqual(result.returncode == 0, not (fail_new or corrupt), result.stdout + result.stderr)
            self.assertEqual((app / "data/config.json").read_text(), "{}")
            with sqlite3.connect(app / "data/tide.sqlite") as db:
                self.assertEqual(db.execute("select id from trades").fetchone()[0], 42)
            if not corrupt:
                with sqlite3.connect(app / "backups/new/tide.sqlite") as db:
                    self.assertEqual(db.execute("select id from trades").fetchone()[0], 42)
            return (root / "restarts").read_text().splitlines() if (root / "restarts").exists() else []

    def test_successful_activation_preserves_data(self):
        self.assertEqual(len(self.run_upgrade()), 1)

    def test_unhealthy_release_restores_previous_code(self):
        self.assertEqual(len(self.run_upgrade(fail_new=True)), 2)

    def test_corrupt_upload_never_restarts_service(self):
        self.assertEqual(self.run_upgrade(corrupt=True), [])


if __name__ == "__main__":
    unittest.main()
