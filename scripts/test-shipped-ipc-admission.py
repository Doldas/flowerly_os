#!/usr/bin/env python3
"""#225 temporary fail-closed default: the shipped distribution admits only
the flowerly service account to the flowerlyd socket.

Until per-user data ownership is enforced, an ordinary flowerly-users member
must not be able to connect, and nothing shipped may re-enable that or make
the service account an implicit administrator. Checked offline against the
files that install-flowerly-rootfs.sh copies into the image, and against
mutated copies so each bypass is proven to be rejected.
"""
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
OVERLAY = ROOT / "distro/overlay"
UNIT = OVERLAY / "usr/lib/systemd/system/flowerlyd.service"
SYSUSERS = OVERLAY / "usr/lib/sysusers.d/flowerly.conf"


def directives(unit_text):
    """Active `Key=value` lines of a unit, comments and sections excluded."""
    pairs = []
    for raw in unit_text.splitlines():
        line = raw.strip()
        if not line or line.startswith(("#", ";", "[")) or "=" not in line:
            continue
        key, value = line.split("=", 1)
        pairs.append((key.strip(), value.strip()))
    return pairs


def admission_problems(unit_text, sysusers_text, overlay_paths):
    problems = []
    pairs = directives(unit_text)
    values = {}
    for key, value in pairs:
        values.setdefault(key, []).append(value)
    environment = " ".join(values.get("Environment", []))
    if "AGENTIC_USERS_GROUP" in environment:
        problems.append("unit sets AGENTIC_USERS_GROUP")
    for key in ("EnvironmentFile", "PassEnvironment", "SupplementaryGroups", "DynamicUser"):
        if key in values:
            problems.append(f"unit sets {key}, which could widen admission or groups")
    if values.get("User") != ["flowerly"]:
        problems.append(f"unit User is {values.get('User')}, expected flowerly")
    if values.get("Group") != ["flowerly"]:
        problems.append(f"unit Group is {values.get('Group')}, expected flowerly")
    if values.get("RuntimeDirectoryMode") != ["0700"]:
        problems.append("RuntimeDirectoryMode must be 0700")
    if values.get("StateDirectoryMode") != ["0700"]:
        problems.append("StateDirectoryMode must be 0700")
    if values.get("UMask") != ["0077"]:
        problems.append("UMask must be 0077")
    if "AGENTIC_ADMIN_GROUP=flowerly-admin" not in environment:
        problems.append("unit must keep AGENTIC_ADMIN_GROUP=flowerly-admin (deny by default)")
    exec_start = " ".join(values.get("ExecStart", []))
    if "--config" in exec_start or "AGENTIC_CONFIG" in environment:
        problems.append("a config file could set users_group")
    for raw in sysusers_text.splitlines():
        fields = raw.split()
        if fields and fields[0] == "m" and len(fields) >= 3 and fields[1] == "flowerly":
            problems.append(f"sysusers adds the service account to {fields[2]}")
    for path in overlay_paths:
        name = path.as_posix()
        if name.endswith(".socket") or re.search(r"flowerlyd\.service\.d/", name):
            problems.append(f"overlay ships {name}, an alternate socket or unit override")
        if name.endswith((".toml", ".conf", ".env")) and path.is_file():
            text = path.read_text(errors="replace")
            active = "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("#"))
            if "users_group" in active or "AGENTIC_USERS_GROUP" in active:
                problems.append(f"{name} configures the users group")
    return problems


class ShippedAdmission(unittest.TestCase):
    def overlay(self):
        return [p.relative_to(OVERLAY) for p in OVERLAY.rglob("*")]

    def test_shipped_files_are_service_account_only(self):
        overlay = [OVERLAY / p for p in self.overlay()]
        problems = admission_problems(UNIT.read_text(), SYSUSERS.read_text(), overlay)
        self.assertEqual(problems, [])

    def test_each_bypass_is_rejected(self):
        unit = UNIT.read_text()
        sysusers = SYSUSERS.read_text()
        env = "Environment=AGENTIC_ADMIN_GROUP=flowerly-admin"
        mutations = {
            "users group": unit.replace(env, env + "\nEnvironment=AGENTIC_USERS_GROUP=flowerly-users"),
            "environment file": unit.replace(env, env + "\nEnvironmentFile=/etc/flowerly/env"),
            "supplementary admin": unit.replace(env, env + "\nSupplementaryGroups=flowerly-admin"),
            "group-owned socket": unit.replace("Group=flowerly", "Group=flowerly-users"),
            "traversable runtime": unit.replace("RuntimeDirectoryMode=0700", "RuntimeDirectoryMode=0750"),
            "no admin group": unit.replace(env, ""),
            "config file": unit.replace("ExecStart=/usr/bin/flowerlyd",
                                        "ExecStart=/usr/bin/flowerlyd --config /etc/f.toml"),
        }
        for label, mutated in mutations.items():
            self.assertNotEqual(mutated, unit, label)
            self.assertTrue(admission_problems(mutated, sysusers, []), label)
        self.assertTrue(admission_problems(unit, sysusers + "m flowerly flowerly-admin\n", []))
        for extra in ("usr/lib/systemd/system/flowerlyd.socket",
                      "usr/lib/systemd/system/flowerlyd.service.d/override.conf"):
            self.assertTrue(admission_problems(unit, sysusers, [pathlib.Path(extra)]), extra)

    def test_a_commented_users_group_is_not_mistaken_for_a_setting(self):
        self.assertIn("AGENTIC_USERS_GROUP", UNIT.read_text())
        self.assertNotIn("AGENTIC_USERS_GROUP",
                         " ".join(v for k, v in directives(UNIT.read_text()) if k == "Environment"))


if __name__ == "__main__":
    unittest.main()
