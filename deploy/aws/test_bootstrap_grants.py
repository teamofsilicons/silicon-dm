"""Exercise both provisioning SQL generators without AWS, passwords, or DB access."""
import ast
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

HERE = Path(__file__).resolve().parent


class BootstrapGrants(unittest.TestCase):
    def generated_sql(self, kind, testing):
        source = (HERE / ("bootstrap-task.py" if kind == "ecs" else "bootstrap.sh")).read_text()
        if kind == "ec2":
            source = source.split("python3 <<'PYTHON'\n", 1)[1].split("\nPYTHON", 1)[0]
        function = next(n for n in ast.parse(source).body
                        if isinstance(n, ast.FunctionDef) and n.name == "configure_database")
        calls = []

        def command(arguments, environment=None, sql=None, **kwargs):
            calls.append(sql if sql is not None else kwargs.get("input", ""))

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            master = {"username": "fixture-admin", "password": "synthetic"}
            (root / "master.json").write_text(json.dumps(master))
            scope = {"os": SimpleNamespace(environ={"BACKEND_IMAGE": "fixture-image"}),
                     "json": json, "root": root, "CA": "fixture-ca", "ca": "fixture-ca",
                     "command": command, "subprocess": SimpleNamespace(run=command),
                     "database_url": lambda *args: "fixture-db", "write_env": lambda *args: None}
            exec(compile(ast.Module(body=[function], type_ignores=[]), "configure_database", "exec"), scope)
            scope["configure_database"](master if kind == "ecs" else "master.json", "fixture-host",
                                        "silicon_dm_test" if testing else "silicon_dm",
                                        "dm_testing" if testing else "dm_runtime", "synthetic", testing=testing)
        return "\n".join(call for call in calls if call)

    def test_testing_migrator_can_create_transaction_local_mapping_tables(self):
        for kind in ["ecs", "ec2"]:
            with self.subTest(kind=kind):
                sql = self.generated_sql(kind, True)
                self.assertIn("GRANT CREATE, TEMPORARY ON DATABASE silicon_dm_test TO dm_testing;", sql)
                self.assertIn("REVOKE ALL ON DATABASE silicon_dm_test FROM PUBLIC;", sql)
                self.assertNotIn("TO PUBLIC", sql)
                self.assertIn("NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOINHERIT NOBYPASSRLS", sql)

    def test_production_role_still_receives_only_database_connect(self):
        for kind in ["ecs", "ec2"]:
            with self.subTest(kind=kind):
                sql = self.generated_sql(kind, False)
                self.assertIn("GRANT CONNECT ON DATABASE silicon_dm TO dm_runtime;", sql)
                self.assertNotIn("TEMPORARY", sql)
                self.assertNotIn("GRANT CREATE", sql)
                self.assertNotIn("TO PUBLIC", sql)


if __name__ == "__main__":
    unittest.main()
