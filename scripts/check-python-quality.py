#!/usr/bin/env python3
"""Fail-closed, incremental Python quality gate for Flowerly CI.

All tracked Python files get a parser check. Changed Python files get Ruff
linting; newly added files also get Ruff formatting. Baseline violations in
untouched legacy files remain visible work, not an excuse to skip new code.
"""
import argparse
import ast
import os
from pathlib import Path
import subprocess
import sys
import tokenize

ROOT = Path(__file__).resolve().parent.parent


def git(root, *args):
    result = subprocess.run(
        ["git", "-C", str(root), *args], capture_output=True, check=True
    )
    return result.stdout


def names(raw):
    return sorted({os.fsdecode(value) for value in raw.split(b"\0") if value})


def python_files(root):
    return names(git(root, "ls-files", "-z", "--", "*.py"))


def changes(root, base, filter_codes):
    git(root, "rev-parse", "--verify", base + "^{commit}")
    return names(
        git(
            root,
            "diff",
            "--name-only",
            "-z",
            "--diff-filter=" + filter_codes,
            base,
            "HEAD",
            "--",
            "*.py",
        )
    )


def check_syntax(root, paths):
    failures = []
    for name in paths:
        path = root / name
        if path.is_symlink() or not path.is_file():
            failures.append(f"{name}: tracked Python file is missing or symlinked")
            continue
        try:
            with tokenize.open(path) as source:
                ast.parse(source.read(), filename=name)
        except (SyntaxError, UnicodeError, OSError, ValueError) as error:
            failures.append(f"{name}: {error}")
    if failures:
        raise ValueError("Python syntax check failed:\n" + "\n".join(failures))
    print(f"Python syntax: {len(paths)} tracked sources parsed", flush=True)


def run_ruff(root, executable, command, paths):
    if not paths:
        return
    cmd = [executable, command]
    if command == "check":
        cmd += ["--no-cache", "--isolated", "--select", "E4,E7,E9,F"]
    else:
        cmd += ["--check"]
    # Use positional paths after '--' to prevent filenames becoming flags.
    cmd += ["--", *paths]
    subprocess.run(cmd, cwd=root, check=True)


def check(root, base, all_files, ruff):
    if not (root / ".git").exists():
        raise ValueError("Python gate requires a complete Git checkout")
    tracked = python_files(root)
    check_syntax(root, tracked)
    if all_files:
        lint = tracked
        fresh = []  # historical formatting is not yet migrated
    else:
        if not base or not base.strip() or base == "0" * 40:
            raise ValueError("exact Git base is required; no silent lint bypass")
        candidates = changes(root, base, "ACMR")
        added = changes(root, base, "A")
        lint = sorted(set(tracked).intersection(candidates))
        fresh = sorted(set(tracked).intersection(added))
    print(
        f"Ruff linting {len(lint)} changed Python files; "
        f"format-checking {len(fresh)} added files",
        flush=True,
    )
    run_ruff(root, ruff, "check", lint)
    run_ruff(root, ruff, "format", fresh)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    selector = parser.add_mutually_exclusive_group(required=True)
    selector.add_argument("--base", help="checked-out commit used for diff")
    selector.add_argument("--all", action="store_true", help="lint all tracked code")
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--ruff", default="ruff")
    args = parser.parse_args(argv)
    try:
        check(args.root.resolve(), args.base, args.all, args.ruff)
    except (
        ValueError,
        OSError,
        subprocess.CalledProcessError,
        FileNotFoundError,
    ) as error:
        print(f"Python quality gate FAILED: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
