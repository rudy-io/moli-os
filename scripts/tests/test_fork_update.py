"""Exercise upstream updates in disposable repositories, never this checkout."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "fork-update.py"


class ForkUpdateTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.upstream = self.root / "upstream"
        self.fork = self.root / "fork"
        self.upstream.mkdir()
        self.env = dict(os.environ, GIT_CONFIG_NOSYSTEM="1",
                        GIT_CONFIG_GLOBAL=os.devnull,
                        GIT_AUTHOR_NAME="Test", GIT_AUTHOR_EMAIL="user@example.com",
                        GIT_COMMITTER_NAME="Test", GIT_COMMITTER_EMAIL="user@example.com",
                        GIT_TERMINAL_PROMPT="0", GIT_MERGE_AUTOEDIT="no")
        self.git(self.upstream, "init", "-b", "main")
        self.commit(self.upstream, "common.txt", "base\n")
        self.git(self.root, "clone", str(self.upstream), str(self.fork))
        self.git(self.fork, "remote", "add", "upstream", str(self.upstream))
        self.git(self.fork, "switch", "-c", "fork/integration")
        self.commit(self.fork, "extension.txt", "fork feature\n")
        self.main = self.git(self.fork, "rev-parse", "main").stdout.strip()

    def git(self, cwd, *args, check=True):
        return subprocess.run(["git", *args], cwd=cwd, env=self.env,
                              text=True, capture_output=True, check=check)

    def commit(self, cwd, file, contents):
        (cwd / file).write_text(contents)
        self.git(cwd, "add", file)
        self.git(cwd, "commit", "-m", f"Update {file}")

    def update(self, *args):
        return subprocess.run(["python3", str(SCRIPT), "--branch", "update/test", *args],
                              cwd=self.fork, env=self.env, text=True, capture_output=True)

    def assert_unchanged_main(self):
        self.assertEqual(self.main, self.git(self.fork, "rev-parse", "main").stdout.strip())

    def test_clean_merge_is_left_for_review_without_commit(self):
        self.commit(self.upstream, "upstream.txt", "new upstream\n")
        previous = self.git(self.fork, "rev-parse", "HEAD").stdout.strip()
        result = self.update()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.git(self.fork, "branch", "--show-current").stdout.strip(), "update/test")
        self.assertEqual(self.git(self.fork, "rev-parse", "HEAD").stdout.strip(), previous)
        self.assertEqual((self.fork / "upstream.txt").read_text(), "new upstream\n")
        self.assertEqual((self.fork / "extension.txt").read_text(), "fork feature\n")
        self.assertEqual(self.git(self.fork, "rev-parse", "--verify", "MERGE_HEAD").returncode, 0)
        self.assert_unchanged_main()

    def test_conflict_stays_resolvable_on_new_branch(self):
        self.commit(self.fork, "common.txt", "fork version\n")
        self.commit(self.upstream, "common.txt", "upstream version\n")
        result = self.update()
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("common.txt", self.git(self.fork, "diff", "--name-only", "--diff-filter=U").stdout)
        self.assert_unchanged_main()

    def test_dirty_tree_is_preserved_and_refused(self):
        (self.fork / "common.txt").write_text("unfinished\n")
        result = self.update()
        self.assertEqual(result.returncode, 1)
        self.assertEqual((self.fork / "common.txt").read_text(), "unfinished\n")
        self.assertEqual(self.git(self.fork, "branch", "--show-current").stdout.strip(), "fork/integration")

    def test_untracked_files_are_preserved_and_refused(self):
        (self.fork / "notes.txt").write_text("private local notes\n")
        self.assertEqual(self.update().returncode, 1)
        self.assertTrue((self.fork / "notes.txt").exists())

    def test_no_update_does_not_create_branch(self):
        result = self.update()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotEqual(self.git(self.fork, "show-ref", "--verify", "refs/heads/update/test", check=False).returncode, 0)
        self.assert_unchanged_main()

    def test_existing_branch_is_not_overwritten(self):
        self.git(self.fork, "branch", "update/test")
        self.commit(self.upstream, "new.txt", "upstream\n")
        before = self.git(self.fork, "rev-parse", "update/test").stdout
        self.assertEqual(self.update().returncode, 1)
        self.assertEqual(self.git(self.fork, "rev-parse", "update/test").stdout, before)

    def test_missing_upstream_does_not_change_working_tree(self):
        self.git(self.fork, "remote", "remove", "upstream")
        self.assertEqual(self.update().returncode, 1)
        self.assertEqual(self.git(self.fork, "status", "--porcelain").stdout, "")

    def test_main_cannot_be_used_as_integration_base(self):
        self.assertEqual(self.update("--base", "main").returncode, 1)
        self.assert_unchanged_main()


if __name__ == "__main__":
    unittest.main()
