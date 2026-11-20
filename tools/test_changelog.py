#!/usr/bin/env python3
"""Unreleased changelog entries live in changelog.d and are moved in at release."""

import importlib.util
import tempfile
import unittest
from pathlib import Path


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


changelog = load("changelog")
RELEASED = "# Changelog\n\n## 0.1.0\n\n- First.\n"


class ChangelogTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name)
        (self.repo / "changelog.d").mkdir()
        self.write("CHANGELOG.md", RELEASED)
        self.write("Cargo.toml", '[package]\nname = "x"\nversion = "0.1.0"\n')
        self.write("changelog.d/released.sha256", changelog.digest(self.repo) + "  CHANGELOG.md\n")

    def write(self, name, text):
        (self.repo / name).write_text(text, encoding="utf-8", newline="\n")

    def test_released_text_with_entries_in_their_own_files_passes(self):
        self.write("changelog.d/one.md", "- One.\n")
        self.assertEqual(changelog.findings(self.repo), [])

    def test_a_section_added_by_hand_is_refused(self):
        for heading in ("Unreleased", "0.1.1"):
            with self.subTest(heading=heading):
                self.write("CHANGELOG.md", RELEASED.replace("## 0.1.0", f"## {heading}\n\n- New.\n\n## 0.1.0"))
                found = changelog.findings(self.repo)
                self.assertEqual(len(found), 1)
                self.assertIn("changelog.d/NAME.md", found[0])

    def test_an_entry_added_to_a_released_section_is_refused(self):
        self.write("CHANGELOG.md", RELEASED + "- New.\n")
        found = changelog.findings(self.repo)
        self.assertEqual(len(found), 1)
        self.assertIn("changelog.d/NAME.md", found[0])

    def test_an_entry_file_holds_only_entries(self):
        for text in ("", "New.\n", "## 0.1.1\n\n- New.\n", "- \n"):
            with self.subTest(text=text):
                self.write("changelog.d/one.md", text)
                self.assertEqual(len(changelog.findings(self.repo)), 1)
        self.write("changelog.d/one.md", "- One.\n")
        self.write("changelog.d/two.markdown", "- Two.\n")
        self.assertIn("two.markdown", changelog.findings(self.repo)[0])

    def test_a_new_version_without_a_release_is_refused(self):
        self.write("Cargo.toml", '[package]\nname = "x"\nversion = "0.1.1" # next\n')
        found = changelog.findings(self.repo)
        self.assertEqual(len(found), 1)
        self.assertIn("tools/changelog.py release", found[0])

    def test_release_moves_entries_word_for_word_and_passes_the_check(self):
        self.write("changelog.d/b.md", "- Second, with `code`.\n")
        self.write("changelog.d/a.md", "- First new.\n- Also first.\n")
        self.write("Cargo.toml", '[package]\nname = "x"\nversion = "0.1.1"\n')
        changelog.release(self.repo)
        self.assertEqual(
            (self.repo / "CHANGELOG.md").read_text(encoding="utf-8"),
            "# Changelog\n\n## 0.1.1\n\n- First new.\n- Also first.\n- Second, with `code`.\n\n## 0.1.0\n\n- First.\n")
        self.assertEqual(changelog.entries(self.repo), [])
        self.assertEqual(changelog.findings(self.repo), [])

    def test_release_refuses_edited_released_text_and_a_malformed_entry(self):
        self.write("Cargo.toml", '[package]\nname = "x"\nversion = "0.1.1"\n')
        self.write("changelog.d/one.md", "- One.\n")
        for name, text in (("CHANGELOG.md", RELEASED.replace("First.", "Changed.")),
                           ("changelog.d/two.md", "## Unreleased\n\nNot an entry.\n")):
            with self.subTest(name=name):
                before = (self.repo / "CHANGELOG.md").read_text(encoding="utf-8") if name != "CHANGELOG.md" else text
                self.write(name, text)
                with self.assertRaises(SystemExit):
                    changelog.release(self.repo)
                self.assertEqual((self.repo / "CHANGELOG.md").read_text(encoding="utf-8"), before)
                self.assertTrue((self.repo / "changelog.d" / "one.md").is_file())
                self.write("CHANGELOG.md", RELEASED)
                (self.repo / "changelog.d" / "two.md").unlink(missing_ok=True)

    def test_release_needs_a_new_version_and_an_entry(self):
        with self.assertRaises(SystemExit):
            changelog.release(self.repo)
        self.write("changelog.d/one.md", "- One.\n")
        with self.assertRaises(SystemExit):
            changelog.release(self.repo)
        (self.repo / "changelog.d" / "one.md").unlink()
        self.write("Cargo.toml", '[package]\nname = "x"\nversion = "0.1.1"\n')
        with self.assertRaises(SystemExit):
            changelog.release(self.repo)
        self.assertEqual((self.repo / "CHANGELOG.md").read_text(encoding="utf-8"), RELEASED)


if __name__ == "__main__":
    unittest.main()
