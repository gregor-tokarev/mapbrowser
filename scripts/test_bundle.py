"""Provisioning rejection and bundle-boundary checks without Keychain signing."""

from datetime import datetime, timedelta, timezone
import hashlib
import io
import json
from pathlib import Path
import plistlib
import re
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import bundle


CERTIFICATE = b"synthetic public certificate bytes for mocked identity listing"
FINGERPRINT = hashlib.sha1(CERTIFICATE).hexdigest().upper()
IDENTITY_NAME = "Apple Development: Browser Tester (ABCDE12345)"
TEAM = "ABCDE12345"
IDENTIFIER = "dev.mapbrowser.browser"
NOW = datetime(2026, 9, 12, tzinfo=timezone.utc)


def valid_profile():
    return {
        "Platform": ["OSX"],
        "ExpirationDate": (NOW + timedelta(days=3650)).replace(tzinfo=None),
        "TeamIdentifier": [TEAM],
        "ApplicationIdentifierPrefix": [TEAM],
        "DeveloperCertificates": [CERTIFICATE],
        "Entitlements": {
            bundle.APP_IDENTIFIER_ENTITLEMENT: f"{TEAM}.{IDENTIFIER}",
            bundle.TEAM_IDENTIFIER_ENTITLEMENT: TEAM,
            bundle.BROWSER_PASSKEY_ENTITLEMENT: True,
        },
    }


def identity_listing(fingerprint=FINGERPRINT, name=IDENTITY_NAME):
    return f'  1) {fingerprint} "{name}"\n     1 valid identities found\n'


class ProfileValidationTests(unittest.TestCase):
    def test_explicit_mac_profile_accepts_selected_certificate(self):
        self.assertEqual(bundle.validate_profile(valid_profile(), IDENTIFIER, FINGERPRINT, NOW), (f"{TEAM}.{IDENTIFIER}", TEAM))

    def test_expired_and_exact_expiry_boundary_rejected(self):
        for delta in (-1, 0):
            with self.subTest(delta=delta):
                profile = valid_profile()
                profile["ExpirationDate"] = NOW + timedelta(seconds=delta)
                with self.assertRaisesRegex(bundle.SigningError, "expired"):
                    bundle.validate_profile(profile, IDENTIFIER, FINGERPRINT, NOW)

    def test_app_team_platform_and_certificate_mismatches_rejected(self):
        cases = [
            ("Platform", ["iOS"], "macOS"),
            ("Platform", "OSX", "Platform"),
            ("ExpirationDate", "tomorrow", "ExpirationDate"),
            ("TeamIdentifier", ["OTHER12345"], "TeamIdentifier"),
            ("ApplicationIdentifierPrefix", ["OTHER12345"], "prefix"),
            ("DeveloperCertificates", [b"another certificate"], "selected signing certificate"),
            ("DeveloperCertificates", [], "DeveloperCertificates"),
            ("DeveloperCertificates", ["not certificate bytes"], "DeveloperCertificates"),
            ("Entitlements", [], "Entitlements"),
        ]
        for key, value, message in cases:
            with self.subTest(key=key, value=value):
                profile = valid_profile()
                profile[key] = value
                with self.assertRaisesRegex(bundle.SigningError, message):
                    bundle.validate_profile(profile, IDENTIFIER, FINGERPRINT, NOW)

    def test_wildcard_wrong_bundle_and_ios_identifier_rejected(self):
        for app_id in [f"{TEAM}.*", f"{TEAM}.dev.another.app", None]:
            with self.subTest(app_id=app_id):
                profile = valid_profile()
                profile["Entitlements"][bundle.APP_IDENTIFIER_ENTITLEMENT] = app_id
                profile["Entitlements"]["application-identifier"] = f"{TEAM}.{IDENTIFIER}"
                with self.assertRaisesRegex(bundle.SigningError, "explicit macOS application identifier"):
                    bundle.validate_profile(profile, IDENTIFIER, FINGERPRINT, NOW)

    def test_capability_requires_boolean_true(self):
        for value in [False, 1, "true", None, []]:
            with self.subTest(value=value):
                profile = valid_profile()
                profile["Entitlements"][bundle.BROWSER_PASSKEY_ENTITLEMENT] = value
                with self.assertRaisesRegex(bundle.SigningError, "must authorize"):
                    bundle.validate_profile(profile, IDENTIFIER, FINGERPRINT, NOW)

    def test_missing_profile_fields_rejected(self):
        for key in valid_profile():
            with self.subTest(key=key):
                profile = valid_profile()
                del profile[key]
                with self.assertRaises(bundle.SigningError):
                    bundle.validate_profile(profile, IDENTIFIER, FINGERPRINT, NOW)

    def test_identity_requires_exact_unique_valid_identity(self):
        listing = identity_listing()
        self.assertEqual(bundle.select_identity(IDENTITY_NAME, listing), FINGERPRINT)
        self.assertEqual(bundle.select_identity(FINGERPRINT.lower(), listing), FINGERPRINT)
        for selection in ["Apple Development", "-", "missing"]:
            with self.subTest(selection=selection), self.assertRaisesRegex(bundle.SigningError, "not a valid installed"):
                bundle.select_identity(selection, listing)
        duplicate = listing + f'  2) {"F" * 40} "{IDENTITY_NAME}"\n'
        with self.assertRaisesRegex(bundle.SigningError, "ambiguous"):
            bundle.select_identity(IDENTITY_NAME, duplicate)
        with self.assertRaisesRegex(bundle.SigningError, "not a valid installed"):
            bundle.select_identity(FINGERPRINT, "0 valid identities found\n")


class SigningPreflightTests(unittest.TestCase):
    def test_default_ad_hoc_does_not_enumerate_keychain(self):
        with patch.object(bundle.subprocess, "run") as external:
            plan = bundle.load_signing_plan(None, None, IDENTIFIER)
        external.assert_not_called()
        self.assertEqual(plan.identity, "-")
        self.assertIsNone(plan.profile_bytes)

    def test_both_options_required_and_missing_file_fails_without_commands(self):
        for identity, profile in [(IDENTITY_NAME, None), (None, Path("profile")), ("-", Path("profile")), (IDENTITY_NAME, Path("/does/not/exist/profile"))]:
            with self.subTest(identity=identity, profile=profile), patch.object(bundle.subprocess, "run") as external:
                with self.assertRaises(bundle.SigningError):
                    bundle.load_signing_plan(identity, profile, IDENTIFIER)
                external.assert_not_called()

    def test_original_cms_bytes_are_pinned_for_embedding(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "Browser.provisionprofile"
            path.write_bytes(b"original synthetic CMS")
            decoded = subprocess.CompletedProcess([], 0, plistlib.dumps(valid_profile()))
            identities = subprocess.CompletedProcess([], 0, identity_listing())
            with patch.object(bundle.subprocess, "run", side_effect=[decoded, identities]) as external:
                plan = bundle.load_signing_plan(IDENTITY_NAME, path, IDENTIFIER)
            path.write_bytes(b"changed file after preflight")
            self.assertEqual(plan.profile_bytes, b"original synthetic CMS")
            self.assertEqual(plan.identity, FINGERPRINT)
            self.assertEqual(external.call_args_list[0].args[0], ["security", "cms", "-D"])
            self.assertEqual(external.call_args_list[0].kwargs["input"], plan.profile_bytes)
            self.assertEqual(external.call_args_list[1].args[0], ["security", "find-identity", "-v", "-p", "codesigning"])

    def test_malformed_cms_and_decoded_plist_fail_clearly(self):
        results = [
            subprocess.CalledProcessError(1, ["security", "cms", "-D"]),
            subprocess.CompletedProcess([], 0, b"not a plist"),
            subprocess.CompletedProcess([], 0, b'<?xml version="1.0"?><plist><dict>'),
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "bad.provisionprofile"
            path.write_bytes(b"synthetic malformed CMS")
            for result in results:
                with self.subTest(result=result), patch.object(bundle.subprocess, "run", side_effect=[result]) as external:
                    with self.assertRaisesRegex(bundle.SigningError, "decode|plist"):
                        bundle.load_signing_plan(IDENTITY_NAME, path, IDENTIFIER)
                    self.assertEqual(external.call_count, 1)

    def test_cli_rejects_invalid_signing_before_build_or_bundle_writes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(bundle, "ROOT", root), patch.object(bundle.sys, "platform", "darwin"), patch.object(bundle.sys, "argv", ["bundle.py", "--sign-identity", IDENTITY_NAME]), patch.object(bundle, "run") as build, patch.object(bundle.subprocess, "check_output") as metadata, patch("sys.stderr", new=io.StringIO()):
                with self.assertRaises(SystemExit) as caught:
                    bundle.main()
            self.assertEqual(caught.exception.code, 2)
            build.assert_not_called()
            metadata.assert_not_called()
            self.assertEqual(list(root.iterdir()), [])


class BundleArtifactTests(unittest.TestCase):
    def test_signed_to_ad_hoc_removes_profile_and_restricted_entitlements(self):
        with tempfile.TemporaryDirectory() as directory:
            app = Path(directory) / "Mapbrowser.app"
            helper = app / "Contents" / "Frameworks" / "Helper.app"
            (helper / "Contents").mkdir(parents=True)
            stale_helper = helper / "Contents" / "embedded.provisionprofile"
            stale_helper.write_bytes(b"stale helper profile")
            signed = bundle.SigningPlan(FINGERPRINT, b"original CMS", f"{TEAM}.{IDENTIFIER}", TEAM)
            main_path, helper_path = bundle.prepare_signing_artifacts(app, [helper], signed)
            self.assertEqual((app / "Contents" / "embedded.provisionprofile").read_bytes(), b"original CMS")
            main = plistlib.loads(main_path.read_bytes())
            helper_entitlements = plistlib.loads(helper_path.read_bytes())
            self.assertEqual(main[bundle.APP_IDENTIFIER_ENTITLEMENT], f"{TEAM}.{IDENTIFIER}")
            self.assertEqual(main[bundle.TEAM_IDENTIFIER_ENTITLEMENT], TEAM)
            self.assertIs(main[bundle.BROWSER_PASSKEY_ENTITLEMENT], True)
            self.assertEqual(helper_entitlements, bundle.JIT_ENTITLEMENTS)
            self.assertFalse(stale_helper.exists())
            main_path, _ = bundle.prepare_signing_artifacts(app, [helper], bundle.SigningPlan())
            self.assertEqual(plistlib.loads(main_path.read_bytes()), bundle.JIT_ENTITLEMENTS)
            self.assertFalse((app / "Contents" / "embedded.provisionprofile").exists())

    def test_every_signed_component_uses_validated_certificate(self):
        app = Path("/bundle/Mapbrowser.app")
        framework = app / "Contents" / "Frameworks" / bundle.FRAMEWORK
        helpers = [framework.parent / "Helper.app", framework.parent / "GPU.app"]
        for plan in [bundle.SigningPlan(), bundle.SigningPlan(FINGERPRINT, b"CMS", f"{TEAM}.{IDENTIFIER}", TEAM)]:
            with self.subTest(identity=plan.identity):
                commands = bundle.signing_commands(app, framework, helpers, plan, Path("main.plist"), Path("helpers.plist"))
                signatures = [command for command in commands if "--sign" in command]
                self.assertEqual({command[-1] for command in signatures}, {str(app), str(framework), *(str(helper) for helper in helpers)})
                self.assertTrue(all(command[command.index("--sign") + 1] == plan.identity for command in signatures))
                self.assertEqual(signatures[-1][-2], "main.plist")
                self.assertTrue(all(command[-2] == "helpers.plist" for command in signatures[1:-1]))
                self.assertIn("--verify", commands[-1])

    def test_bundle_main_registers_http_https_and_helpers_do_not(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / "target"
            profile = target / "debug"
            runtime = root / "cef"
            (runtime / bundle.FRAMEWORK).mkdir(parents=True)
            (runtime / "CREDITS.html").write_text("Synthetic test credits")
            output = profile / "build" / "cef-dll-sys-test" / "output"
            output.parent.mkdir(parents=True)
            output.write_text(f"cargo:metadata=CEF_DIR={runtime}\n")
            (profile / "mapbrowser").write_bytes(b"synthetic main executable")
            (profile / "mapbrowser-helper").write_bytes(b"synthetic helper executable")
            write_filter_lists(root)
            with patch.object(bundle, "ROOT", root), patch.object(bundle.sys, "platform", "darwin"), patch.object(bundle.sys, "argv", ["bundle.py", "--skip-build", "--name", "Browser Test", "--identifier", "dev.test.browser"]), patch.object(bundle.subprocess, "check_output", return_value=json.dumps({"target_directory": str(target)}).encode()), patch.object(bundle, "run") as commands, patch.object(bundle.subprocess, "run") as external, patch("sys.stdout", new=io.StringIO()):
                bundle.main()
            external.assert_not_called()
            app = target / "bundle" / "Browser Test.app"
            main_info = plistlib.loads((app / "Contents" / "Info.plist").read_bytes())
            registration = main_info["CFBundleURLTypes"][0]
            self.assertEqual(registration["CFBundleURLSchemes"], ["http", "https"])
            self.assertEqual(registration["CFBundleTypeRole"], "Viewer")
            self.assertEqual(registration["LSHandlerRank"], "Alternate")
            self.assertEqual(main_info["CFBundleIdentifier"], "dev.test.browser")
            helpers = list((app / "Contents" / "Frameworks").glob("*.app"))
            self.assertEqual(len(helpers), 5)
            for helper in helpers:
                helper_info = plistlib.loads((helper / "Contents" / "Info.plist").read_bytes())
                self.assertNotIn("CFBundleURLTypes", helper_info)
                self.assertTrue(helper_info["CFBundleIdentifier"].startswith("dev.test.browser.helper"))
            signatures = [call.args for call in commands.call_args_list if call.args[0] == "codesign" and "--sign" in call.args]
            self.assertTrue(all(command[command.index("--sign") + 1] == "-" for command in signatures))
            filters = app / "Contents" / "Resources" / "filters"
            self.assertEqual(sorted(path.name for path in filters.iterdir()), ["easylist.txt", "easyprivacy.txt"])
            self.assertEqual((filters / "easylist.txt").read_text(), "||ads.example^\n")


class FilterListTests(unittest.TestCase):
    def test_lists_replace_stale_bundle_copies(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_filter_lists(root)
            (root / "resources" / "filters" / "notes.md").write_text("not a list")
            resources = root / "Mapbrowser.app" / "Contents" / "Resources"
            (resources / "filters").mkdir(parents=True)
            (resources / "filters" / "removed-list.txt").write_text("||stale.example^\n")
            with patch.object(bundle, "ROOT", root):
                bundle.copy_filter_lists(resources)
            self.assertEqual(sorted(path.name for path in (resources / "filters").iterdir()), ["easylist.txt", "easyprivacy.txt"])

    def test_missing_lists_stop_the_bundle(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(bundle, "ROOT", root), self.assertRaisesRegex(SystemExit, "content-blocking lists"):
                bundle.copy_filter_lists(root / "Resources")

    def test_every_list_the_app_loads_is_required(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            filters = root / "resources" / "filters"
            filters.mkdir(parents=True)
            (filters / "easylist.txt").write_text("||ads.example^\n")
            # A renamed download must not pass for the real list.
            (filters / "easyprivacy-2026.txt").write_text("||tracker.example^\n")
            with patch.object(bundle, "ROOT", root), self.assertRaises(SystemExit) as stopped:
                bundle.copy_filter_lists(root / "Resources")
            self.assertIn("easyprivacy.txt", str(stopped.exception))
            self.assertNotIn("easylist.txt", str(stopped.exception))
            self.assertFalse((root / "Resources" / "filters").exists())

    def test_bundle_lists_match_the_app(self):
        source = (Path(bundle.__file__).resolve().parent.parent / "src" / "blocker.rs").read_text()
        lists = re.search(r"const LISTS: \[&str; \d+\] = \[(.*?)\];", source).group(1)
        self.assertEqual(tuple(re.findall(r'"([^"]+)"', lists)), bundle.FILTER_LISTS)

    def test_repository_ships_easylist_and_easyprivacy(self):
        lists = Path(bundle.__file__).resolve().parent.parent / "resources" / "filters"
        for name, title in [("easylist.txt", "EasyList"), ("easyprivacy.txt", "EasyPrivacy")]:
            with self.subTest(name=name):
                header = (lists / name).read_text().splitlines()[:5]
                self.assertIn(f"! Title: {title}", header)


def write_filter_lists(root):
    filters = root / "resources" / "filters"
    filters.mkdir(parents=True)
    (filters / "easylist.txt").write_text("||ads.example^\n")
    (filters / "easyprivacy.txt").write_text("||tracker.example^\n")


if __name__ == "__main__":
    unittest.main()
