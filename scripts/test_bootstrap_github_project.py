"""Offline regression tests; no GitHub credentials or external writes required."""
import importlib.util
import pathlib
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "bootstrap", pathlib.Path(__file__).with_name("bootstrap-github-project.py"))
bootstrap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bootstrap)


class BootstrapTests(unittest.TestCase):
    def test_custom_category_does_not_create_reserved_type(self):
        with patch.object(bootstrap, "gh"), patch.object(
                bootstrap, "ensure_project", return_value=(7, "PVT_7")), patch.object(
                bootstrap, "ensure_field") as ensure, patch.object(
                bootstrap, "ensure_views"), patch.object(bootstrap, "sync") as sync:
            bootstrap.main(["--project", "7"])
        names = [c.args[1] for c in ensure.call_args_list]
        self.assertIn("Work type", names)
        self.assertNotIn("Type", names)
        sync.assert_called_once_with(7, "PVT_7")

    def test_hidden_native_type_does_not_replace_custom_category(self):
        native = {"id": "native_type", "name": "Type", "type": "ProjectV2Field"}
        with patch.object(bootstrap, "data", side_effect=[
                {"fields": [native]}, {"id": "custom_type"}]) as data:
            bootstrap.ensure_field(7, "Work type", "SINGLE_SELECT", ["Feature"])
        create = data.call_args_list[1].args
        self.assertEqual(create[0:2], ("project", "field-create"))
        self.assertEqual(create[create.index("--name") + 1], "Work type")

    def test_reuses_case_insensitive_existing_field(self):
        field = {"id": "work_type", "name": "work TYPE",
                 "type": "ProjectV2SingleSelectField",
                 "options": [{"id": "feature", "name": "Feature"}]}
        with patch.object(bootstrap, "data", return_value={"fields": [field]}) as data:
            self.assertEqual(bootstrap.ensure_field(
                7, "Work type", "SINGLE_SELECT", ["Feature"]), field)
        self.assertEqual(data.call_count, 1)

    def test_incompatible_options_fail_without_mutation(self):
        field = {"name": "Work type", "type": "ProjectV2SingleSelectField",
                 "options": [{"id": "task", "name": "Task"}]}
        with patch.object(bootstrap, "data", return_value={"fields": [field]}) as data:
            with self.assertRaisesRegex(RuntimeError, "missing options: Feature"):
                bootstrap.ensure_field(7, "Work type", "SINGLE_SELECT", ["Feature"])
        self.assertEqual(data.call_count, 1)

    def test_explicit_project_reuses_board_and_rejects_wrong_title(self):
        with patch.object(bootstrap, "gh"), patch.object(bootstrap, "data", side_effect=[
                {"title": bootstrap.TITLE, "number": 7}, {"id": "PVT_7"}]) as data:
            self.assertEqual(bootstrap.ensure_project(7), (7, "PVT_7"))
        self.assertTrue(all(c.args[:2] == ("project", "view")
                            for c in data.call_args_list))
        with patch.object(bootstrap, "data", return_value={"title": "Unrelated"}), patch.object(
                bootstrap, "gh") as gh:
            with self.assertRaisesRegex(RuntimeError, "is not"):
                bootstrap.ensure_project(7)
        gh.assert_not_called()

    def test_failed_import_resumes_missing_fields_and_preserves_edits(self):
        issue = {"number": 8, "title": "[Roadmap] 0.0.4", "state": "OPEN"}
        url = "https://github.com/" + bootstrap.REPO + "/issues/8"
        values = bootstrap.fields(issue, False)
        schema = [{"name": key, "id": "field_" + str(i),
                   "options": [{"name": value, "id": "option_" + str(i)}]}
                  for i, (key, value) in enumerate(values.items())]
        by_id = {field["id"]: field for field in schema}
        state = {}
        calls = []
        fail_once = [True]

        def data(*args):
            if args[:2] == ("issue", "list"):
                return [issue]
            if args[:2] == ("pr", "list"):
                return []
            if args[:2] == ("project", "field-list"):
                return {"fields": schema}
            if args[:2] == ("project", "item-add"):
                calls.append(args)
                state[url] = {"id": "PVTI_8", "fieldValues": {"nodes": []}}
                return state[url]
            raise AssertionError(args)

        def gh(*args):
            calls.append(args)
            self.assertEqual(args[:2], ("project", "item-edit"))
            self.assertEqual(args[args.index("--id") + 1], "PVTI_8")
            self.assertEqual(args[args.index("--project-id") + 1], "PVT_7")
            field_id = args[args.index("--field-id") + 1]
            field = by_id[field_id]
            if field["name"] == "Delivery stage" and fail_once[0]:
                fail_once[0] = False
                raise RuntimeError("simulated network failure")
            self.assertIn("--single-select-option-id", args)
            state[url]["fieldValues"]["nodes"].append(
                {"field": {"id": field_id}, "name": "Human-selected value"})

        with patch.object(bootstrap, "data", side_effect=data), patch.object(
                bootstrap, "gh", side_effect=gh), patch.object(
                bootstrap, "existing_items", side_effect=lambda _: state):
            with self.assertRaisesRegex(RuntimeError, "simulated"):
                bootstrap.sync(7, "PVT_7")
            bootstrap.sync(7, "PVT_7")
            count_after_recovery = len(calls)
            bootstrap.sync(7, "PVT_7")
        self.assertEqual(len(calls), count_after_recovery)
        self.assertEqual(sum(c[:2] == ("project", "item-add") for c in calls), 1)
        self.assertEqual(sum("field_0" in c for c in calls), 1)
        self.assertEqual(len(state[url]["fieldValues"]["nodes"]), 6)

    def test_existing_items_paginates_and_ignores_drafts(self):
        page1 = {"data": {"node": {"items": {"nodes": [
            {"id": "draft", "content": None},
            {"id": "one", "content": {"url": "https://example/one"}}],
            "pageInfo": {"hasNextPage": True, "endCursor": "cursor_1"}}}}}
        page2 = {"data": {"node": {"items": {"nodes": [
            {"id": "two", "content": {"url": "https://example/two"}}],
            "pageInfo": {"hasNextPage": False, "endCursor": None}}}}}
        with patch.object(bootstrap, "data", side_effect=[page1, page2]) as data:
            items = bootstrap.existing_items("PVT_7")
        self.assertEqual(set(items), {"https://example/one", "https://example/two"})
        self.assertIn("cursor=cursor_1", data.call_args_list[1].args)


if __name__ == "__main__":
    unittest.main()
