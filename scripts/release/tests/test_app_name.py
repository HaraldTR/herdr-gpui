"""macOS shows an unbundled process under its executable name, so every
macOS launch path must present the bundle named Herdr, not herdr-gpui."""
from pathlib import Path
import plistlib
import re
import unittest


ROOT = Path(__file__).resolve().parents[3]


class AppNameTests(unittest.TestCase):
    def test_bundle_plist_names_the_app_herdr(self):
        info = plistlib.loads((ROOT / "assets/macos/Info.plist").read_bytes())
        for key in ("CFBundleName", "CFBundleDisplayName", "CFBundleExecutable"):
            self.assertEqual(info[key], "Herdr", key)

    def test_development_recipes_launch_the_bundled_executable(self):
        justfile = (ROOT / "justfile").read_text()
        for recipe, profile in (("run", "release"), ("run-debug", "debug")):
            body = re.search(rf"(?m)^{re.escape(recipe)} \*args:\n((?:[ \t]+.*\n|\n)+)", justfile)
            self.assertIsNotNone(body, recipe)
            launch = f"exec target/{profile}/Herdr.app/Contents/MacOS/Herdr {{{{args}}}}"
            self.assertIn(launch, body.group(1))
            self.assertNotIn(f"target/{profile}/herdr-gpui", body.group(1))
            if recipe == "run":
                self.assertIn("{{just_executable()}} bundle release qa-menu", body.group(1))
                self.assertIn("--features qa-menu -- {{args}}", body.group(1))

    def test_bundle_recipe_installs_the_executable_and_icon_as_herdr(self):
        justfile = (ROOT / "justfile").read_text()
        body = re.search(r'(?m)^bundle profile="release" features="":\n((?:[ \t]+.*\n|\n)+)', justfile)
        self.assertIsNotNone(body)
        for expected in (
            'cargo build --locked $flags --target-dir target -p herdr-gpui --features "{{features}}"',
            'cp target/{{profile}}/herdr-gpui "$app/Contents/MacOS/Herdr"',
            'cp assets/macos/Info.plist "$app/Contents/Info.plist"',
            '"$app/Contents/Resources/Herdr.icns"',
        ):
            self.assertIn(expected, body.group(1))
