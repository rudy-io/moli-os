#!/usr/bin/env python3
"""Prepare a reviewable upstream merge; never commit, push, reset or rebase."""

import argparse
import subprocess
import sys


def git(*args, check=True):
    return subprocess.run(["git", *args], text=True, capture_output=True, check=check)


def prepare(base, branch):
    # Restrict the base to a local product branch; main remains an upstream mirror.
    if base in {"main", "master"} or not base.startswith("fork/"):
        raise ValueError("Use a local fork/* integration branch as --base; keep main pristine.")
    if not branch.startswith("update/"):
        raise ValueError("Use a new update/* branch name.")
    git("check-ref-format", f"refs/heads/{base}")
    git("check-ref-format", f"refs/heads/{branch}")
    git("rev-parse", "--show-toplevel")
    if git("status", "--porcelain", "--untracked-files=normal").stdout:
        raise ValueError("Working tree is not clean; commit or stash your work first.")
    # A no-change merge/rebase can have a clean porcelain status.
    for ref in ("MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "REBASE_HEAD"):
        if git("rev-parse", "--verify", "--quiet", ref, check=False).returncode == 0:
            raise ValueError("Finish the current Git operation before preparing an update.")
    git("show-ref", "--verify", f"refs/heads/{base}")
    if git("show-ref", "--verify", f"refs/heads/{branch}", check=False).returncode == 0:
        raise ValueError(f"Branch {branch} already exists; it will not be overwritten.")
    git("remote", "get-url", "upstream")
    git("fetch", "--no-tags", "upstream", "refs/heads/main")
    target = git("rev-parse", "FETCH_HEAD").stdout.strip()
    ancestor = git("merge-base", "--is-ancestor", target, f"refs/heads/{base}", check=False)
    if ancestor.returncode == 0:
        print(f"{base} already contains upstream/main; no branch created.")
        return 0
    if ancestor.returncode != 1:
        raise ValueError("Could not compare integration history with upstream/main.")
    git("switch", "-c", branch, f"refs/heads/{base}")
    merged = git("merge", "--no-commit", "--no-ff", target, check=False)
    if merged.returncode:
        print(merged.stderr or merged.stdout, file=sys.stderr)
        print(f"Update left on {branch} for inspection. Resolve the merge or run git merge --abort.", file=sys.stderr)
        return 2
    print(f"Upstream {target} merged into {branch}, without a commit.")
    print("Review the diff, run the project checks, then commit and open a PR to fork/integration.")
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="fork/integration", help="Local fork/* branch (default: fork/integration)")
    parser.add_argument("--branch", required=True, help="New update/* branch to prepare")
    args = parser.parse_args()
    try:
        return prepare(args.base, args.branch)
    except (ValueError, subprocess.CalledProcessError) as error:
        # Do not print remote URLs: they may contain credentials.
        message = str(error) if isinstance(error, ValueError) else "Git operation failed; inspect repository state and remote access."
        print(message, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
