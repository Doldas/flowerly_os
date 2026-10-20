#!/usr/bin/env python3
"""Keep the supported Makefile fallback aligned with canonical just recipes.

Uses only the standard library, no Rust build or dependencies. Fail closed on
unrecognized recipes rather than trusting names or silently normalizing away
different build flags. A separate check tests docs against actual paths.
"""
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parent.parent
MAKE = (ROOT / "Makefile").read_text()
JUST = (ROOT / "justfile").read_text()
BUILD = (ROOT / "web-docs/docs/build.md").read_text()
CHANGELOG = (ROOT / "CHANGELOG.md").read_text()

SHARED = ("fmt", "fmt-check", "lint", "check", "build", "test", "verify",
          "run", "serve", "stop", "status", "release", "image-inputs",
          "image", "vm", "vm-test")


def recipes(text, flavor):
    lines = text.splitlines()
    found = {}
    for index, line in enumerate(lines):
        if flavor == "make":
            match = re.fullmatch(r"([a-z][a-z0-9-]*):(?:\s+(.*))?", line)
            if not match:
                continue
            name = match[1]
            deps = (match[2] or "").split()
            commands = []
            for following in lines[index + 1:]:
                if following.startswith("\t"):
                    commands.append(following.strip())
                else:
                    break
        else:
            match = re.fullmatch(r"([a-z][a-z0-9-]*):(?:\s+(.+))?", line)
            if not match:
                continue
            name = match[1]
            deps = (match[2] or "").split()
            commands = []
            for following in lines[index + 1:]:
                if following.startswith("    "):
                    commands.append(following.strip())
                else:
                    break
        if name in found:
            raise AssertionError(f"{flavor}: duplicate recipe {name}")
        found[name] = (deps, commands)
    return found


make = recipes(MAKE, "make")
just = recipes(JUST, "just")
for name in SHARED:
    assert name in make, f"Makefile missing {name}"
    assert name in just, f"justfile missing {name}"

# These recipes must execute exactly the same Cargo operations in the same
# order. Make's explicit CARGO override expands to cargo by default.
for name in ("fmt", "fmt-check", "lint", "check", "build", "test"):
    mdeps, mcmd = make[name]
    jdeps, jcmd = just[name]
    mcmd = [c.replace("$(CARGO)", "cargo") for c in mcmd]
    assert (mdeps, mcmd) == (jdeps, jcmd), (
        f"{name}: Makefile {(mdeps, mcmd)} != justfile {(jdeps, jcmd)}"
    )

# Verify the resulting command sequence rather than requiring identical
# internal dependency graphs (Make additionally runs a cargo check).
assert just["verify"][0] == ["lint", "test"], just["verify"]
assert just["verify"][1] == ["cargo build --workspace --release"]
assert make["verify"][0] == ["lint", "check"], make["verify"]
assert make["verify"][1] == [
    "$(CARGO) test --workspace",
    "$(CARGO) build --workspace --release",
], make["verify"]
assert just["run"][0] == ["serve"] and make["run"][0] == ["serve"]
assert just["serve"][0] == ["build"] and make["serve"][0] == ["build"]
assert just["stop"][0] == ["servestop"]
assert just["servestop"][1] == ["./scripts/dev-runtime.sh stop"]
assert make["stop"][1] == ["./scripts/dev-runtime.sh stop"]
assert just["status"][1] == ["./scripts/dev-runtime.sh status"]
assert make["status"][1] == ["./scripts/dev-runtime.sh status"]
assert "check-image-inputs.sh" in " ".join(make["image-inputs"][1])
assert "check-image-inputs.sh" in " ".join(just["image-inputs"][1])
for name in ("image", "vm", "vm-test"):
    for flavor, recipeset in (("make", make), ("just", just)):
        joined = "\n".join(recipeset[name][1])
        assert (
            "exit 1" in joined
            or "exit 1" in joined.replace("@", "")
        ), f"{flavor} {name} must fail closed until implemented"
assert "web-docs/docs/releases.md" in MAKE, "release doc link must use actual case"
assert (ROOT / "web-docs/docs/releases.md").is_file()
assert "docs/RELEASES.md" not in MAKE
assert "cargo run -p flowd" not in BUILD, "old daemon name in build guide"
assert "image/arch/" not in BUILD, "outdated distro input path"
assert "just build    # build the workspace (release)" in BUILD
assert "## Unreleased" in CHANGELOG
assert "## 0.0.1" in CHANGELOG
print("Build runner parity, fail-closed targets and documentation links: OK")
