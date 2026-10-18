#!/usr/bin/env python3
"""Sync every issue and PR into the Flowerly OS engineering project board.

Adds missing items and fills unset project fields (Target release, Delivery
stage, Work type, Area, Priority, Sprint, Owner). Populated fields are never
overwritten, so human edits in the GitHub UI are preserved. Safe to rerun.

Usage (GitHub CLI authenticated with 'project' scope):
  gh auth refresh -s project
  python3 scripts/sync-project.py

CI usage: the same script runs with --project from the repository default.
"""
import importlib.util
import pathlib
import sys

_BOOTSTRAP = pathlib.Path(__file__).with_name("bootstrap-github-project.py")
_spec = importlib.util.spec_from_file_location("bootstrap", _BOOTSTRAP)
assert _spec is not None and _spec.loader is not None
_bootstrap = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_bootstrap)


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    if "--project" not in argv:
        argv += ["--project", "7"]
    argv.append("--sync-only")
    _bootstrap.main(argv)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, KeyError, ValueError) as e:
        print("Project sync failed:", e, file=sys.stderr)
        sys.exit(1)
