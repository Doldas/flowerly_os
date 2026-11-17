#!/usr/bin/env python3
"""Offline unit tests for the Arch VM resource timeline."""

import csv
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "arch_vm_resource_monitor", Path(__file__).with_name("arch-vm-resource-monitor.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class ResourceMonitorTests(unittest.TestCase):
    def test_missing_pidfile_is_not_live(self):
        with tempfile.TemporaryDirectory() as tmp:
            row = module.sample(str(Path(tmp) / "missing"), proc=tmp, cgroup=tmp)
        self.assertEqual(row["qemu_alive"], 0)
        self.assertIsNone(row["qemu_rss_kib"])

    def test_reject_pid_injection(self):
        with tempfile.TemporaryDirectory() as tmp:
            pidfile = Path(tmp) / "qemu.pid"
            pidfile.write_text("12;cat /etc/passwd", encoding="ascii")
            self.assertIsNone(module.qemu_pid(pidfile))

    def test_reads_numeric_proc_and_cgroup_metrics(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "qemu.pid").write_text("42", encoding="ascii")
            (root / "42").mkdir()
            (root / "42" / "stat").write_text("42 (qemu) S", encoding="ascii")
            (root / "42" / "status").write_text("VmRSS:\t1200 kB\n", encoding="ascii")
            (root / "meminfo").write_text(
                "MemAvailable: 2048 kB\nSwapFree: 4096 kB\n", encoding="ascii"
            )
            (root / "memory.current").write_text("67108864\n", encoding="ascii")
            (root / "memory.events").write_text("oom 2\noom_kill 1\n", encoding="ascii")
            row = module.sample(root / "qemu.pid", proc=root, cgroup=root)
            self.assertEqual(row["qemu_alive"], 1)
            self.assertEqual(row["qemu_rss_kib"], 1200)
            self.assertEqual(row["host_mem_available_kib"], 2048)
            self.assertEqual(row["host_swap_free_kib"], 4096)
            self.assertEqual(row["cgroup_memory_current_bytes"], 67108864)
            self.assertEqual(row["cgroup_oom_kill"], 1)

    def test_cpu_and_dimm_temperatures_come_from_named_hwmon_chips_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)

            def chip(directory, name, **temps):
                (root / directory).mkdir()
                (root / directory / "name").write_text(name + "\n", encoding="ascii")
                for key, value in temps.items():
                    (root / directory / key).write_text(f"{value}\n", encoding="ascii")

            chip("hwmon1", "k10temp", temp1_input=61250, temp3_input=58000)
            chip("hwmon2", "spd5118", temp1_input=45000)
            chip("hwmon3", "spd5118", temp1_input=52500)
            chip("hwmon4", "nvme", temp1_input=99000)
            chip("hwmon5", "amdgpu", temp1_input=88000)
            row = module.sample(root / "none", proc=root, cgroup=root, hwmon=root)
            self.assertEqual(row["cpu_temp_mc"], 61250)
            self.assertEqual(row["dimm_temp_max_mc"], 52500)

    def test_missing_unreadable_or_absurd_temperatures_are_blank(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            row = module.sample(
                root / "none", proc=root, cgroup=root, hwmon=root / "absent"
            )
            self.assertIsNone(row["cpu_temp_mc"])
            self.assertIsNone(row["dimm_temp_max_mc"])
            (root / "hwmon1").mkdir()
            (root / "hwmon1" / "name").write_text("k10temp\n", encoding="ascii")
            (root / "hwmon1" / "temp1_input").write_text("garbage\n", encoding="ascii")
            (root / "hwmon1" / "temp2_input").write_text(
                "999999999\n", encoding="ascii"
            )
            row = module.sample(root / "none", proc=root, cgroup=root, hwmon=root)
            self.assertIsNone(row["cpu_temp_mc"])

    def test_unreadable_metrics_are_unknown_not_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "meminfo").write_text(
                "MemAvailable: invalid kB\n", encoding="ascii"
            )
            row = module.sample(root / "qemu.pid", proc=root, cgroup=root)
            self.assertIsNone(row["host_mem_available_kib"])
            self.assertIsNone(row["cgroup_oom"])

    def test_csv_is_bounded_and_only_allowlisted_columns(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "timeline.csv"
            with patch.object(
                module,
                "sample",
                return_value={
                    "qemu_alive": 1,
                    "qemu_rss_kib": 12,
                    "host_mem_available_kib": 100,
                    "host_swap_free_kib": 200,
                    "cgroup_memory_current_bytes": 99,
                    "cgroup_oom": 0,
                    "cgroup_oom_kill": 0,
                },
            ):
                with patch.object(module.time, "sleep", return_value=None):
                    module.monitor("missing", output, interval=1, limit=3)
            with output.open(newline="", encoding="ascii") as stream:
                rows = list(csv.DictReader(stream))
            self.assertEqual(len(rows), 3)
            self.assertEqual(set(rows[0]), set(module.COLUMNS))

    def test_does_not_overwrite_existing_evidence(self):
        with tempfile.TemporaryDirectory() as tmp:
            output = Path(tmp) / "timeline.csv"
            output.write_text("keep", encoding="ascii")
            with self.assertRaises(FileExistsError):
                module.monitor("missing", output, limit=1)
            self.assertEqual(output.read_text(encoding="ascii"), "keep")


if __name__ == "__main__":
    unittest.main()
