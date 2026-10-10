"""Exercise both provisioning SQL generators without AWS, passwords, or DB access."""
import ast
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent


def bootstrap_source(kind):
    """The Python of the ECS bootstrap task, or the Python embedded in the EC2 user data."""
    source = (HERE / ("bootstrap-task.py" if kind == "ecs" else "bootstrap.sh")).read_text()
    if kind == "ec2":
        source = source.split("python3 <<'PYTHON'\n", 1)[1].split("\nPYTHON", 1)[0]
    return source


class BootstrapGrants(unittest.TestCase):
    def generated(self, kind):
        """(SQL statements, other commands) that configure_database issues."""
        sql, commands = [], []

        def command(arguments, environment=None, sql_text=None, **kwargs):
            text = sql_text if sql_text is not None else kwargs.get("input")
            (sql if text else commands).append(text or arguments)

        if kind == "ecs":
            spec = importlib.util.spec_from_file_location("bootstrap_task_grants", HERE / "bootstrap-task.py")
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            master = {"username": "fixture-admin", "password": "synthetic"}
            with mock.patch.object(module, "command", command):
                module.configure_database(master, "fixture-host", "silicon_dm", "dm_runtime", "synthetic")
            return "\n".join(sql), commands
        function = next(n for n in ast.parse(bootstrap_source(kind)).body
                        if isinstance(n, ast.FunctionDef) and n.name == "configure_database")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            master = {"username": "fixture-admin", "password": "synthetic"}
            (root / "master.json").write_text(json.dumps(master))
            scope = {"os": SimpleNamespace(environ={"BACKEND_IMAGE": "fixture-image"}),
                     "json": json, "root": root, "ca": "fixture-ca",
                     "subprocess": SimpleNamespace(run=command),
                     "database_url": lambda *args: "fixture-db", "write_env": lambda *args: None}
            exec(compile(ast.Module(body=[function], type_ignores=[]), "configure_database", "exec"), scope)
            scope["configure_database"]("master.json", "fixture-host", "silicon_dm", "dm_runtime", "synthetic")
        return "\n".join(sql), commands

    def test_production_role_receives_only_database_connect(self):
        for kind in ["ecs", "ec2"]:
            with self.subTest(kind=kind):
                sql, _ = self.generated(kind)
                self.assertIn("GRANT CONNECT ON DATABASE silicon_dm TO dm_runtime;", sql)
                self.assertIn("REVOKE ALL ON DATABASE silicon_dm FROM PUBLIC;", sql)
                self.assertIn("NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOINHERIT NOBYPASSRLS", sql)
                self.assertNotIn("TEMPORARY", sql)
                self.assertNotIn("GRANT CREATE", sql)
                self.assertNotIn("TO PUBLIC", sql)

    def test_migrations_and_runtime_grants_follow_the_role(self):
        for kind in ["ecs", "ec2"]:
            with self.subTest(kind=kind):
                _, commands = self.generated(kind)
                flat = [" ".join(map(str, arguments)) for arguments in commands]
                migrate = next(i for i, line in enumerate(flat) if line.endswith("dm-migrate"))
                grants = next(i for i, line in enumerate(flat) if "runtime_role=dm_runtime" in line)
                self.assertLess(migrate, grants)

    def test_neither_bootstrap_touches_the_retained_testing_database(self):
        for kind in ["ecs", "ec2"]:
            with self.subTest(kind=kind):
                source = (HERE / ("bootstrap-task.py" if kind == "ecs" else "bootstrap.sh")).read_text()
                for retired in ["TEST_DB_", "silicon_dm_test", "dm_testing", "testing=True"]:
                    self.assertNotIn(retired, source)


if __name__ == "__main__":
    unittest.main()
