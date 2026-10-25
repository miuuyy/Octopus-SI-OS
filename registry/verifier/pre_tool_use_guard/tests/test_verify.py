import importlib.util
import io
import json
from contextlib import redirect_stdout
from pathlib import Path
import sys
import unittest

spec = importlib.util.spec_from_file_location("pre_tool_use_guard", Path(__file__).parents[1] / "verify.py")
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def run(payload, **params):
    request = {"artifact_label": "tool.call", "payload_utf8": json.dumps(payload), "params": params}
    out = io.StringIO()
    stdin = sys.stdin
    sys.stdin = io.StringIO(json.dumps(request))
    try:
        with redirect_stdout(out):
            guard.main()
    finally:
        sys.stdin = stdin
    return json.loads(out.getvalue())


def exec_call(command):
    return {"tool": "coder.exec", "command": command}


class DeniedSubstrings(unittest.TestCase):
    def test_denies_a_listed_substring_as_before(self):
        self.assertFalse(run(exec_call("rm -rf /"), denied_command_substrings=["rm -rf"])["passed"])
        self.assertTrue(run(exec_call("ls"), denied_command_substrings=["rm -rf"])["passed"])

    def test_a_reordered_flag_is_not_the_listed_substring(self):
        # The limit the README now states: a substring list matches text, not effect.
        self.assertTrue(run(exec_call("rm -fr /"), denied_command_substrings=["rm -rf"])["passed"])


class AllowedCommands(unittest.TestCase):
    ALLOWED = ["git", "ls", "cargo"]

    def check(self, command, **extra):
        return run(exec_call(command), allowed_commands=self.ALLOWED, **extra)

    def test_admits_a_plain_listed_command(self):
        for command in ["git status", "ls -la", "cargo build --locked", "git log --format=%h"]:
            with self.subTest(command=command):
                self.assertTrue(self.check(command)["passed"])

    def test_refuses_a_command_that_is_not_listed(self):
        for command in ["rm -fr /", "unlink file", "find . -delete", "python3 x.py"]:
            with self.subTest(command=command):
                verdict = self.check(command)
                self.assertFalse(verdict["passed"])
                self.assertIn("allowed_commands", verdict["details"]["error"])

    def test_refuses_any_shell_feature_in_a_command_string(self):
        for command in ["git status; rm -fr /", "ls && rm x", "ls | sh", "ls $(rm x)", "ls `rm x`",
                        "git log > out", "ls\nrm x", "l''s", "\\ls", "ls ${IFS}x", "ls 'a b'"]:
            with self.subTest(command=command):
                self.assertFalse(self.check(command)["passed"])

    def test_refuses_a_path_instead_of_a_name(self):
        self.assertFalse(self.check("/bin/ls")["passed"])
        self.assertFalse(run(exec_call(["/usr/bin/git", "status"]), allowed_commands=self.ALLOWED)["passed"])

    def test_checks_the_first_element_of_an_argv_array(self):
        self.assertTrue(run(exec_call(["git", "status"]), allowed_commands=self.ALLOWED)["passed"])
        self.assertFalse(run(exec_call(["sh", "-c", "git status"]), allowed_commands=self.ALLOWED)["passed"])

    def test_argv_arguments_are_not_shell_parsed(self):
        # An argv array runs without a shell, so a metacharacter in an argument is just text.
        self.assertTrue(run(exec_call(["git", "commit", "-m", "fix; tidy"]), allowed_commands=self.ALLOWED)["passed"])

    def test_reads_the_command_under_arguments_too(self):
        payload = {"tool": "coder.exec", "arguments": {"command": "git status"}}
        self.assertTrue(run(payload, allowed_commands=self.ALLOWED)["passed"])
        payload = {"tool": "coder.exec", "arguments": {"cmd": "rm x"}}
        self.assertFalse(run(payload, allowed_commands=self.ALLOWED)["passed"])

    def test_fails_closed_on_an_empty_or_malformed_command(self):
        for command in ["", "   ", [], [""], [1, 2], {"bin": "ls"}]:
            with self.subTest(command=command):
                self.assertFalse(run(exec_call(command), allowed_commands=self.ALLOWED)["passed"])

    def test_leaves_a_call_without_a_command_to_the_other_checks(self):
        payload = {"tool": "coder.read_file", "arguments": {"path": "src/lib.rs"}}
        self.assertTrue(run(payload, allowed_commands=self.ALLOWED)["passed"])
        self.assertFalse(run(payload, allowed_commands=self.ALLOWED, denied_tools=["coder.read_file"])["passed"])

    def test_rejects_a_malformed_allowlist(self):
        for value in ["git", [""], ["git", 3], ["/usr/bin/git"]]:
            with self.subTest(value=value):
                self.assertFalse(run(exec_call("git status"), allowed_commands=value)["passed"])

    def test_still_applies_denied_substrings_alongside(self):
        verdict = run(exec_call("git push --force"), allowed_commands=self.ALLOWED, denied_command_substrings=["--force"])
        self.assertFalse(verdict["passed"])


if __name__ == "__main__":
    unittest.main()
