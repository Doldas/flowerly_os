#!/usr/bin/env python3
"""Wait for completed exact-head release push evidence before validating master."""
import importlib.util
from pathlib import Path
import sys
import time
from urllib import parse

spec = importlib.util.spec_from_file_location(
    "flowerly_branch_forward", Path(__file__).with_name("branch-forward.py"))
forward = importlib.util.module_from_spec(spec)
spec.loader.exec_module(forward)

WORKFLOWS = ("ci.yml", "security.yml", "codeql.yml")


def await_promotion(head, sha, timeout=6600, interval=30, client=None, clock=None, pause=None):
    client = client or forward.api
    clock = clock or time.monotonic
    pause = pause or time.sleep
    deadline = clock() + timeout
    while True:
        # The existing validator checks the latest branch, exact SHA, all three
        # successful push runs, rust/docs, and the executed Arch acceptance step.
        try:
            return forward.verify_master(head, sha, client)
        except ValueError as exc:
            if "no successful completed PUSH run" not in str(exc):
                raise
            pending = []
            for workflow in WORKFLOWS:
                path = (f"/actions/workflows/{workflow}/runs?branch="
                        f"{parse.quote(head, safe='')}&event=push&per_page=100")
                runs = client("GET", path).get("workflow_runs", [])
                matching = [r for r in runs if r.get("head_sha") == sha
                            and r.get("head_branch") == head
                            and r.get("event") == "push"]
                if any(r.get("status") == "completed"
                       and r.get("conclusion") != "success" for r in matching):
                    raise ValueError(f"{workflow}: exact-SHA PUSH run completed unsuccessfully") from exc
                if not any(r.get("status") == "completed"
                           and r.get("conclusion") == "success" for r in matching):
                    pending.append(workflow)
            if clock() >= deadline:
                raise ValueError(f"timed out waiting for exact-SHA release push: {pending}") from exc
            print(f"Waiting for exact-SHA push checks on {head}@{sha}: {pending}",
                  flush=True)
            pause(interval)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: wait-release-promotion.py release/N 40-hex-sha")
    try:
        await_promotion(sys.argv[1], sys.argv[2])
    except ValueError as exc:
        sys.exit(f"promotion verification failed: {exc}")
