#!/usr/bin/env python3
"""Unit tests for cross-boundary FK and missing core-table detection (#82)."""

import unittest
from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path

path = Path(__file__).with_name("audit-storage-schema.py")
spec = spec_from_file_location("storage_schema_audit", path)
module = module_from_spec(spec)
spec.loader.exec_module(module)


class StorageSchemaAuditTests(unittest.TestCase):
    def test_core_tables_required(self):
        self.assertTrue(any("missing core" in p for p in module.audit([], [])))

    def test_cross_boundary_reference_is_rejected(self):
        tables = module.CORE_REQUIRED | module.MEMORY_CANDIDATES
        edges = [("user_facts", "flows", "flow_id", "id")]
        self.assertTrue(any("cross-boundary" in p for p in module.audit(tables, edges)))

    def test_reverse_core_to_memory_fk_is_also_rejected(self):
        tables = module.CORE_REQUIRED | module.MEMORY_CANDIDATES
        self.assertTrue(
            any(
                "cross-boundary" in p
                for p in module.audit(
                    tables, [("flows", "user_facts", "memory_id", "id")]
                )
            )
        )

    def test_core_internal_references_are_allowed_in_inventory(self):
        tables = module.CORE_REQUIRED | module.MEMORY_CANDIDATES
        edges = [("sessions", "flows", "flow_id", "id")]
        self.assertEqual(module.audit(tables, edges), [])


if __name__ == "__main__":
    unittest.main()
