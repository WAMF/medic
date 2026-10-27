import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("release", Path(__file__).with_name("check-release-version.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseTest(unittest.TestCase):
    def test_tag_matches_package(self):
        release.check("v0.1.0", '[package]\nversion = "0.1.0"')

    def test_wrong_tag_fails(self):
        for tag in ("v0.2.0", "0.1.0", "main", "v0.1.0/extra"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release.check(tag, '[package]\nversion = "0.1.0"')
