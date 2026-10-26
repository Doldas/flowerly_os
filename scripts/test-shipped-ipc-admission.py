#!/usr/bin/env python3
"""#225: shipped flowerlyd admission. Service-account-only by default,
multi-user only through the maintained opt-in drop-in.

The default the image ships (flowerlyd.service plus sysusers) admits only the
flowerly service account to the socket. Nothing shipped may re-enable a users
group, activate a drop-in or alternate socket, or make the service account an
implicit administrator.

The opt-in drop-in (usr/share/flowerly/systemd/flowerlyd-multi-user.conf) is
staged inactive. Merged over the unit with systemd's rules, it must give
exactly: users group flowerly-users, primary group flowerly-users (socket
0660), a 0750 runtime directory, a still-private 0700 state directory and
0077 umask, the unchanged admin group, and still no manager-injected config
or database path (#237).

Checked offline against the files that install-flowerly-rootfs.sh copies into
the image, and against mutated copies so each bypass is proven to be
rejected. The real systemd behavior is accepted in the Arch guest by
test-systemd-multi-user.py.
"""
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
OVERLAY = ROOT / "distro/overlay"
UNIT = OVERLAY / "usr/lib/systemd/system/flowerlyd.service"
SYSUSERS = OVERLAY / "usr/lib/sysusers.d/flowerly.conf"
DROP_IN_PATH = pathlib.Path("usr/share/flowerly/systemd/flowerlyd-multi-user.conf")
DROP_IN = OVERLAY / DROP_IN_PATH

# Directives that are lists in systemd: assignments accumulate and an empty
# assignment resets the list. Every other directive here is last-one-wins.
LIST_KEYS = {"Environment", "UnsetEnvironment", "ExecStart"}


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


def effective(*texts):
    """Merge a unit and drop-ins in order, as systemd does."""
    values = {}
    for text in texts:
        for key, value in directives(text):
            if key in LIST_KEYS:
                values[key] = [] if value == "" else values.get(key, []) + [value]
            else:
                values[key] = [value]
    return values


def environment_of(values):
    env = {}
    for entry in values.get("Environment", []):
        for item in entry.split():
            name, _, setting = item.partition("=")
            env[name] = setting
    return env


def unset_of(values):
    return " ".join(values.get("UnsetEnvironment", [])).split()


def common_problems(values):
    """Invariants of every supported configuration."""
    problems = []
    env = environment_of(values)
    for key in ("EnvironmentFile", "PassEnvironment", "SupplementaryGroups", "DynamicUser"):
        if key in values:
            problems.append(f"sets {key}, which could widen admission or groups")
    if values.get("User") != ["flowerly"]:
        problems.append(f"User is {values.get('User')}, expected flowerly")
    if values.get("StateDirectoryMode") != ["0700"]:
        problems.append("StateDirectoryMode must be 0700 (state.db stays private)")
    if values.get("UMask") != ["0077"]:
        problems.append("UMask must be 0077")
    if env.get("AGENTIC_ADMIN_GROUP") != "flowerly-admin":
        problems.append("AGENTIC_ADMIN_GROUP must stay flowerly-admin (deny by default)")
    if "AGENTIC_CONFIG" in env:
        problems.append("sets AGENTIC_CONFIG: a config file could set users_group")
    if "AGENTIC_CONFIG" not in unset_of(values):
        problems.append("must UnsetEnvironment=AGENTIC_CONFIG so the manager cannot inject it")
    # #237: the manager must not repoint the system database (/dev/shm, a
    # second file); tests and recovery run the binary directly.
    if "AGENTIC_DATABASE_PATH" in env:
        problems.append("sets AGENTIC_DATABASE_PATH: the database must stay in the state directory")
    if "AGENTIC_DATABASE_PATH" not in unset_of(values):
        problems.append("must UnsetEnvironment=AGENTIC_DATABASE_PATH so the manager cannot inject it")
    exec_start = " ".join(values.get("ExecStart", []))
    if exec_start != "/usr/bin/flowerlyd --state-dir /var/lib/flowerly":
        problems.append(f"ExecStart changed: {exec_start!r}")
    return problems


def default_problems(values):
    problems = common_problems(values)
    if "AGENTIC_USERS_GROUP" in environment_of(values):
        problems.append("default sets AGENTIC_USERS_GROUP")
    if "AGENTIC_USERS_GROUP" not in unset_of(values):
        problems.append("default must UnsetEnvironment=AGENTIC_USERS_GROUP")
    if values.get("Group") != ["flowerly"]:
        problems.append(f"default Group is {values.get('Group')}, expected flowerly")
    if values.get("RuntimeDirectoryMode") != ["0700"]:
        problems.append("default RuntimeDirectoryMode must be 0700")
    return problems


def opt_in_problems(values):
    problems = common_problems(values)
    env = environment_of(values)
    if env.get("AGENTIC_USERS_GROUP") != "flowerly-users":
        problems.append(f"opt-in users group is {env.get('AGENTIC_USERS_GROUP')!r}")
    if "AGENTIC_USERS_GROUP" in unset_of(values):
        problems.append("opt-in still unsets AGENTIC_USERS_GROUP (it would never apply)")
    if values.get("Group") != ["flowerly-users"]:
        problems.append(f"opt-in Group is {values.get('Group')}, expected flowerly-users")
    if values.get("RuntimeDirectoryMode") != ["0750"]:
        problems.append("opt-in RuntimeDirectoryMode must be 0750")
    return problems


def drop_in_problems(drop_in_text):
    """The drop-in may only change what multi-user access needs."""
    allowed = {"Group", "RuntimeDirectoryMode", "UnsetEnvironment", "Environment"}
    problems = [f"drop-in sets {key}" for key, _ in directives(drop_in_text)
                if key not in allowed]
    names = [item.partition("=")[0] for key, value in directives(drop_in_text)
             if key == "Environment" for item in value.split()]
    if names != ["AGENTIC_USERS_GROUP"]:
        problems.append(f"drop-in Environment must set only AGENTIC_USERS_GROUP: {names}")
    return problems


def overlay_problems(sysusers_text, overlay_paths):
    problems = []
    for raw in sysusers_text.splitlines():
        fields = raw.split()
        if fields and fields[0] == "m" and len(fields) >= 3 and fields[1] == "flowerly":
            problems.append(f"sysusers adds the service account to {fields[2]}")
    for path in overlay_paths:
        name = path.as_posix()
        if name.endswith(".socket") or re.search(r"\.service\.d(/|$)", name):
            problems.append(f"overlay ships {name}, an alternate socket or active unit override")
        if path == DROP_IN_PATH:
            continue
        absolute = OVERLAY / path
        if name.endswith((".toml", ".conf", ".env")) and absolute.is_file():
            text = absolute.read_text(errors="replace")
            active = "\n".join(l for l in text.splitlines() if not l.lstrip().startswith("#"))
            if any(k in active for k in ("users_group", "AGENTIC_USERS_GROUP")):
                problems.append(f"{name} configures the users group")
    return problems


class ShippedAdmission(unittest.TestCase):
    def overlay(self):
        return [p.relative_to(OVERLAY) for p in OVERLAY.rglob("*")]

    def test_shipped_default_is_service_account_only(self):
        self.assertEqual(default_problems(effective(UNIT.read_text())), [])
        self.assertEqual(overlay_problems(SYSUSERS.read_text(), self.overlay()), [])
        self.assertIn(DROP_IN_PATH, self.overlay())

    def test_the_drop_in_gives_exactly_the_opt_in_policy(self):
        drop_in = DROP_IN.read_text()
        self.assertEqual(drop_in_problems(drop_in), [])
        self.assertEqual(opt_in_problems(effective(UNIT.read_text(), drop_in)), [])
        # Removing the drop-in is the rollback: the unit alone is the default.
        self.assertTrue(default_problems(effective(UNIT.read_text(), drop_in)))

    def test_each_default_bypass_is_rejected(self):
        unit = UNIT.read_text()
        env = "Environment=AGENTIC_ADMIN_GROUP=flowerly-admin"
        unset = "UnsetEnvironment=AGENTIC_USERS_GROUP AGENTIC_CONFIG AGENTIC_DATABASE_PATH"
        self.assertIn(unset, unit)
        mutations = {
            "users group": unit.replace(env, env + "\nEnvironment=AGENTIC_USERS_GROUP=flowerly-users"),
            "environment file": unit.replace(env, env + "\nEnvironmentFile=/etc/flowerly/env"),
            "supplementary admin": unit.replace(env, env + "\nSupplementaryGroups=flowerly-admin"),
            "group-owned socket": unit.replace("Group=flowerly", "Group=flowerly-users"),
            "traversable runtime": unit.replace("RuntimeDirectoryMode=0700", "RuntimeDirectoryMode=0750"),
            "readable state": unit.replace("StateDirectoryMode=0700", "StateDirectoryMode=0750"),
            "loose umask": unit.replace("UMask=0077", "UMask=0022"),
            "no admin group": unit.replace(env, ""),
            "inherits manager users group": unit.replace(
                unset, "UnsetEnvironment=AGENTIC_CONFIG AGENTIC_DATABASE_PATH"),
            "inherits manager config": unit.replace(
                unset, "UnsetEnvironment=AGENTIC_USERS_GROUP AGENTIC_DATABASE_PATH"),
            "inherits manager database path": unit.replace(
                unset, "UnsetEnvironment=AGENTIC_USERS_GROUP AGENTIC_CONFIG"),
            "sets database path": unit.replace(
                env, env + "\nEnvironment=AGENTIC_DATABASE_PATH=/dev/shm/state.db"),
            "config file": unit.replace("ExecStart=/usr/bin/flowerlyd",
                                        "ExecStart=/usr/bin/flowerlyd --config /etc/f.toml"),
            "root": unit.replace("User=flowerly", "User=root"),
        }
        for label, mutated in mutations.items():
            self.assertNotEqual(mutated, unit, label)
            self.assertTrue(default_problems(effective(mutated)), label)
        self.assertTrue(overlay_problems(SYSUSERS.read_text() + "m flowerly flowerly-admin\n", []))
        for extra in ("usr/lib/systemd/system/flowerlyd.socket",
                      "usr/lib/systemd/system/flowerlyd.service.d/override.conf",
                      "etc/systemd/system/flowerlyd.service.d/50-multi-user.conf",
                      "usr/lib/systemd/system/flowerlyd.service.d"):
            self.assertTrue(overlay_problems("", [pathlib.Path(extra)]), extra)

    def test_each_drop_in_weakening_is_rejected(self):
        unit = UNIT.read_text()
        drop_in = DROP_IN.read_text()
        users = "Environment=AGENTIC_USERS_GROUP=flowerly-users"
        self.assertIn(users, drop_in)
        mutations = {
            "keeps unit unset list": drop_in.replace("UnsetEnvironment=\n", ""),
            "drops config unset": drop_in.replace("UnsetEnvironment=AGENTIC_CONFIG AGENTIC_DATABASE_PATH\n",
                                                  "UnsetEnvironment=AGENTIC_DATABASE_PATH\n"),
            "drops database unset": drop_in.replace("UnsetEnvironment=AGENTIC_CONFIG AGENTIC_DATABASE_PATH\n",
                                                    "UnsetEnvironment=AGENTIC_CONFIG\n"),
            "database path": drop_in.replace(users, users + "\nEnvironment=AGENTIC_DATABASE_PATH=/dev/shm/x.db"),
            "other users group": drop_in.replace(users, "Environment=AGENTIC_USERS_GROUP=wheel"),
            "no users group": drop_in.replace(users, ""),
            "admin group swap": drop_in.replace(users, users + "\nEnvironment=AGENTIC_ADMIN_GROUP=wheel"),
            "admin as users": drop_in.replace(users, users + " AGENTIC_ADMIN_GROUP=flowerly-users"),
            "config": drop_in.replace(users, users + "\nEnvironment=AGENTIC_CONFIG=/etc/f.toml"),
            "supplementary admin": drop_in + "SupplementaryGroups=flowerly-admin\n",
            "readable state": drop_in + "StateDirectoryMode=0750\n",
            "loose umask": drop_in + "UMask=0022\n",
            "environment file": drop_in + "EnvironmentFile=/etc/flowerly/env\n",
            "root": drop_in + "User=root\n",
            "exec": drop_in + "ExecStart=\nExecStart=/usr/bin/flowerlyd --config /etc/f.toml\n",
            "world runtime": drop_in.replace("RuntimeDirectoryMode=0750", "RuntimeDirectoryMode=0755"),
            "private group": drop_in.replace("Group=flowerly-users", "Group=flowerly"),
        }
        for label, mutated in mutations.items():
            self.assertNotEqual(mutated, drop_in, label)
            self.assertTrue(drop_in_problems(mutated) or
                            opt_in_problems(effective(unit, mutated)), label)

    def test_a_commented_users_group_is_not_mistaken_for_a_setting(self):
        self.assertIn("AGENTIC_USERS_GROUP", UNIT.read_text())
        self.assertNotIn("AGENTIC_USERS_GROUP", environment_of(effective(UNIT.read_text())))

    def test_merge_follows_systemd_list_and_reset_rules(self):
        merged = effective("[Service]\nUnsetEnvironment=A B\nGroup=x\n",
                           "[Service]\nUnsetEnvironment=\nUnsetEnvironment=B\nGroup=y\n")
        self.assertEqual(unset_of(merged), ["B"])
        self.assertEqual(merged["Group"], ["y"])


if __name__ == "__main__":
    unittest.main()
