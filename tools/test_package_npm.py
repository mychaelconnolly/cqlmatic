"""Check versioned install instructions in the extracted npm package."""
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location("package_npm", Path(__file__).with_name("package-npm.py"))
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


class NpmGuideTests(unittest.TestCase):
    def test_current_install_command_is_required_without_stale_commands(self):
        PACKAGE.install_guide(b"npx cqlmatic@0.1.3 setup\n", "0.1.3")
        with self.assertRaisesRegex(ValueError, "Current npm install instructions missing"):
            PACKAGE.install_guide(b"npx cqlmatic@0.1.1 setup\n", "0.1.3")
        with self.assertRaisesRegex(ValueError, "Stale npm install instructions"):
            PACKAGE.install_guide(b"npx cqlmatic@0.1.3 setup\nnpx cqlmatic@0.1.1 check\n", "0.1.3")


if __name__ == "__main__":
    unittest.main()
