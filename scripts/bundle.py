#!/usr/bin/env python3
"""Build a macOS app; use ad hoc signing unless a profile and identity are supplied."""
from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
from xml.parsers.expat import ExpatError

ROOT = Path(__file__).resolve().parent.parent
FRAMEWORK = "Chromium Embedded Framework.framework"
# Rendered from assets/AppIcon.svg by scripts/make_icon.py.
APP_ICON = "AppIcon.icns"
# Files Finder, the Dock and `open -a` may open with the app: the kinds a
# browser shows, and web locations (.webloc). The app is an alternative
# for them, never their owner.
DOCUMENT_TYPES = (
    ("Web page", ["public.html", "public.xhtml"]),
    ("PDF document", ["com.adobe.pdf"]),
    ("Web location", ["com.apple.web-internet-location"]),
    ("SVG image", ["public.svg-image"]),
    ("Image", ["public.png", "public.jpeg", "com.compuserve.gif", "org.webmproject.webp", "public.avif", "com.microsoft.bmp"]),
    ("Plain text", ["public.plain-text"]),
)
# Keep in step with LISTS in src/blocker.rs.
FILTER_LISTS = ("easylist.txt", "easyprivacy.txt")
BROWSER_PASSKEY_ENTITLEMENT = "com.apple.developer.web-browser.public-key-credential"
APP_IDENTIFIER_ENTITLEMENT = "com.apple.application-identifier"
TEAM_IDENTIFIER_ENTITLEMENT = "com.apple.developer.team-identifier"
JIT_ENTITLEMENTS = {
    "com.apple.security.cs.allow-jit": True,
    "com.apple.security.cs.allow-unsigned-executable-memory": True,
    "com.apple.security.cs.disable-library-validation": True,
}
# The hardened runtime refuses these devices to apps without them, even
# after the user allows a site. macOS asks the app (Chromium's capture
# helpers act for it), so only the app carries them, as in Chrome.
DEVICE_ENTITLEMENTS = {
    "com.apple.security.device.camera": True,
    "com.apple.security.device.audio-input": True,
    "com.apple.security.personal-information.location": True,
}


class SigningError(ValueError):
    """An explicitly requested signing configuration failed preflight."""


@dataclass(frozen=True)
class SigningPlan:
    identity: str = "-"
    profile_bytes: bytes | None = None
    app_identifier: str | None = None
    team_identifier: str | None = None

    def main_entitlements(self):
        result = {**JIT_ENTITLEMENTS, **DEVICE_ENTITLEMENTS}
        if self.profile_bytes is not None:
            result.update({
                APP_IDENTIFIER_ENTITLEMENT: self.app_identifier,
                TEAM_IDENTIFIER_ENTITLEMENT: self.team_identifier,
                BROWSER_PASSKEY_ENTITLEMENT: True,
            })
        return result


def select_identity(requested, listing):
    """Resolve a unique valid installed identity and pin its certificate SHA-1."""
    identities = re.findall(r'^\s*\d+\)\s+([0-9A-Fa-f]{40})\s+"([^"\n]+)"\s*$', listing, re.MULTILINE)
    matches = {
        fingerprint.upper()
        for fingerprint, name in identities
        if requested.upper() == fingerprint.upper() or requested == name
    }
    if not matches:
        raise SigningError(
            "The selected signing identity is not a valid installed code-signing identity. "
            "Use its full SHA-1 fingerprint or exact name from security find-identity -v -p codesigning."
        )
    if len(matches) != 1:
        raise SigningError("The signing identity name is ambiguous; select its full SHA-1 fingerprint.")
    return matches.pop()


def validate_profile(profile, bundle_identifier, identity, now=None):
    """Validate the profile's macOS app, team, capability, expiry and signing cert."""
    if not isinstance(profile, dict):
        raise SigningError("The provisioning profile must decode to a plist dictionary.")
    platforms = profile.get("Platform")
    if not isinstance(platforms, list) or not all(isinstance(item, str) for item in platforms):
        raise SigningError("The provisioning profile has no valid Platform array.")
    if not {"OSX", "macOS"}.intersection(platforms):
        raise SigningError("The provisioning profile does not support macOS.")
    expiration = profile.get("ExpirationDate")
    if not isinstance(expiration, datetime):
        raise SigningError("The provisioning profile has no valid ExpirationDate.")
    expiration = expiration.replace(tzinfo=timezone.utc) if expiration.tzinfo is None else expiration.astimezone(timezone.utc)
    current = now or datetime.now(timezone.utc)
    current = current.replace(tzinfo=timezone.utc) if current.tzinfo is None else current.astimezone(timezone.utc)
    if expiration <= current:
        raise SigningError("The provisioning profile has expired; select a current macOS profile.")
    entitlements = profile.get("Entitlements")
    if not isinstance(entitlements, dict):
        raise SigningError("The provisioning profile has no Entitlements dictionary.")
    if entitlements.get(BROWSER_PASSKEY_ENTITLEMENT) is not True:
        raise SigningError(f"The provisioning profile must authorize {BROWSER_PASSKEY_ENTITLEMENT} as true.")
    team = entitlements.get(TEAM_IDENTIFIER_ENTITLEMENT)
    teams = profile.get("TeamIdentifier")
    if not isinstance(team, str) or not re.fullmatch(r"[A-Z0-9]{10}", team):
        raise SigningError("The provisioning profile has no valid team-identifier entitlement.")
    if not isinstance(teams, list) or not all(isinstance(item, str) for item in teams) or team not in teams:
        raise SigningError("The provisioning profile team entitlement does not match TeamIdentifier.")
    prefixes = profile.get("ApplicationIdentifierPrefix")
    if not isinstance(prefixes, list) or not all(isinstance(item, str) for item in prefixes) or team not in prefixes:
        raise SigningError("The macOS profile App ID prefix must match its team identifier.")
    app_identifier = entitlements.get(APP_IDENTIFIER_ENTITLEMENT)
    expected = f"{team}.{bundle_identifier}"
    if not isinstance(app_identifier, str) or "*" in app_identifier or app_identifier != expected:
        raise SigningError(f"The profile must authorize the explicit macOS application identifier {expected}.")
    certificates = profile.get("DeveloperCertificates")
    if not isinstance(certificates, list) or not certificates or not all(isinstance(item, bytes) and item for item in certificates):
        raise SigningError("The provisioning profile has no valid DeveloperCertificates array.")
    # security/codesign identify certificates by SHA-1, independently of the
    # algorithms used by the certificate or the application's code signature.
    fingerprints = {hashlib.sha1(certificate).hexdigest().upper() for certificate in certificates}
    if identity not in fingerprints:
        raise SigningError("The selected signing certificate is not in this profile's DeveloperCertificates.")
    return app_identifier, team


def load_signing_plan(sign_identity, provisioning_profile, bundle_identifier):
    if sign_identity is None and provisioning_profile is None:
        return SigningPlan()
    if not sign_identity or not provisioning_profile:
        raise SigningError("Supply both --sign-identity and --provisioning-profile to opt into provisioned signing.")
    if sign_identity == "-":
        raise SigningError("A provisioning profile requires a real signing identity, not ad hoc '-'.")
    try:
        # Decode and later embed the same bytes, even if the source file changes
        # while Cargo builds. security cms reads stdin when -i is absent.
        profile_bytes = Path(provisioning_profile).expanduser().read_bytes()
    except OSError as error:
        raise SigningError(f"Cannot read provisioning profile: {error}") from error
    if not profile_bytes:
        raise SigningError("The provisioning profile file is empty.")
    try:
        decoded = subprocess.run(
            ["security", "cms", "-D"], input=profile_bytes, capture_output=True, check=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise SigningError("security cms could not decode the provisioning profile. Select an original Apple-issued macOS profile.") from error
    try:
        profile = plistlib.loads(decoded.stdout)
    except (ValueError, TypeError, OverflowError, plistlib.InvalidFileException, ExpatError) as error:
        raise SigningError("The decoded provisioning profile is not a valid plist.") from error
    try:
        installed = subprocess.run(
            ["security", "find-identity", "-v", "-p", "codesigning"], capture_output=True, text=True, check=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        raise SigningError("Could not list valid installed code-signing identities with security find-identity.") from error
    identity = select_identity(sign_identity, installed.stdout)
    app_identifier, team = validate_profile(profile, bundle_identifier, identity)
    return SigningPlan(identity, profile_bytes, app_identifier, team)


def prepare_signing_artifacts(app, helpers, plan):
    helper_entitlements = app.parent / "entitlements.plist"
    main_entitlements = app.parent / f"{app.stem}.entitlements.plist"
    write_plist(helper_entitlements, JIT_ENTITLEMENTS)
    write_plist(main_entitlements, plan.main_entitlements())
    embedded = app / "Contents" / "embedded.provisionprofile"
    if plan.profile_bytes is None:
        embedded.unlink(missing_ok=True)
    else:
        embedded.write_bytes(plan.profile_bytes)
    for helper in helpers:
        (helper / "Contents" / "embedded.provisionprofile").unlink(missing_ok=True)
    return main_entitlements, helper_entitlements


def signing_commands(app, framework, helpers, plan, main_entitlements, helper_entitlements):
    commands = [("codesign", "--force", "--deep", "--sign", plan.identity, str(framework))]
    for helper in helpers:
        commands.append(("codesign", "--force", "--sign", plan.identity, "--options", "runtime", "--entitlements", str(helper_entitlements), str(helper)))
    commands.append(("codesign", "--force", "--sign", plan.identity, "--options", "runtime", "--entitlements", str(main_entitlements), str(app)))
    commands.append(("codesign", "--verify", "--deep", "--strict", str(app)))
    return commands


def run(*args):
    subprocess.run(args, cwd=ROOT, check=True)


def copy_filter_lists(resources):
    """Bundle the content-blocking lists, replacing any from earlier builds.

    The app loads exactly these lists and turns blocking off if one is missing,
    so a bundle without both must not be built.
    """
    source = ROOT / "resources" / "filters"
    missing = [name for name in FILTER_LISTS if not (source / name).is_file()]
    if missing:
        raise SystemExit(f"Missing content-blocking lists in resources/filters: {', '.join(missing)}")
    destination = resources / "filters"
    shutil.rmtree(destination, ignore_errors=True)
    destination.mkdir()
    for name in FILTER_LISTS:
        shutil.copy2(source / name, destination / name)


def write_plist(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as f:
        plistlib.dump(value, f)


def package_version(metadata):
    """The app's version, from Cargo.toml through cargo metadata."""
    for package in metadata.get("packages", []):
        if package.get("name") == "mapbrowser":
            return package["version"]
    return "0.0.0"


def document_types():
    return [
        {
            "CFBundleTypeName": name,
            "CFBundleTypeRole": "Viewer",
            "LSHandlerRank": "Alternate",
            "LSItemContentTypes": types,
        }
        for name, types in DOCUMENT_TYPES
    ]


def copy_app_icon(resources):
    """Copies the icon into `resources`; returns whether there was one."""
    icon = ROOT / "assets" / APP_ICON
    if not icon.is_file():
        print(f"warning: {icon} is missing; the app will have the generic icon", file=sys.stderr)
        return False
    shutil.copy2(icon, resources / APP_ICON)
    return True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true")
    parser.add_argument("--skip-build", action="store_true")
    parser.add_argument("--name", default="Map Browser")
    parser.add_argument("--identifier", default="dev.mapbrowser.browser")
    parser.add_argument("--sign-identity", help="Opt in with a valid installed identity's exact name or full SHA-1 fingerprint; requires --provisioning-profile")
    parser.add_argument("--provisioning-profile", type=Path, help="Apple-issued macOS profile for this explicit app ID with the browser passkey entitlement; requires --sign-identity")
    args = parser.parse_args()
    if sys.platform != "darwin":
        raise SystemExit("Map Browser currently supports macOS only.")
    try:
        signing = load_signing_plan(args.sign_identity, args.provisioning_profile, args.identifier)
    except SigningError as error:
        parser.error(str(error))
    if not args.skip_build:
        run("cargo", "build", "--bins", *(["--release"] if args.release else []))
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version=1"], cwd=ROOT))
    target = Path(metadata["target_directory"])
    profile = target / ("release" if args.release else "debug")
    cef_dir = None
    outputs = sorted((profile / "build").glob("cef-dll-sys-*/output"), key=lambda p: p.stat().st_mtime, reverse=True)
    for output in outputs:
        for line in output.read_text().splitlines():
            if "metadata=CEF_DIR=" in line:
                candidate = Path(line.split("metadata=CEF_DIR=", 1)[1])
                if (candidate / FRAMEWORK).exists():
                    cef_dir = candidate
                    break
        if cef_dir:
            break
    if cef_dir is None:
        raise SystemExit("Cannot find CEF runtime. Run cargo build successfully first.")
    app = target / "bundle" / (args.name + ".app")
    contents = app / "Contents"
    frameworks = contents / "Frameworks"
    macos = contents / "MacOS"
    macos.mkdir(parents=True, exist_ok=True)
    frameworks.mkdir(parents=True, exist_ok=True)
    (contents / "Resources").mkdir(exist_ok=True)
    credits = cef_dir / "CREDITS.html"
    if not credits.is_file():
        raise SystemExit("The CEF runtime is missing CREDITS.html.")
    shutil.copy2(credits, contents / "Resources" / "CREDITS.html")
    if (ROOT / "licenses").is_dir():
        shutil.copytree(ROOT / "licenses", contents / "Resources" / "licenses", dirs_exist_ok=True)
    copy_filter_lists(contents / "Resources")
    has_icon = copy_app_icon(contents / "Resources")
    version = package_version(metadata)
    shutil.copy2(profile / "mapbrowser", macos / "mapbrowser")
    destination = frameworks / FRAMEWORK
    # rsync updates binaries and preserves framework symlinks on repeat builds.
    run("rsync", "-a", "--delete", str(cef_dir / FRAMEWORK) + "/", str(destination) + "/")
    info = {
        "CFBundleName": args.name,
        "CFBundleDisplayName": args.name,
        "CFBundleIdentifier": args.identifier,
        "CFBundleExecutable": "mapbrowser",
        "CFBundlePackageType": "APPL",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleVersion": version,
        "CFBundleShortVersionString": version,
        "LSMinimumSystemVersion": "12.0",
        "NSHighResolutionCapable": True,
        "NSSupportsAutomaticGraphicsSwitching": True,
        "NSPrincipalClass": "GPUIApplication",
        "LSFileQuarantineEnabled": True,
        "LSEnvironment": {"MallocNanoZone": "0"},
        "NSCameraUsageDescription": "Allow websites you choose to use the camera.",
        "NSMicrophoneUsageDescription": "Allow websites you choose to use the microphone.",
        # Chromium locates through Core Location, which needs these to ask.
        "NSLocationUsageDescription": "Allow websites you choose to know your location.",
        "NSLocationWhenInUseUsageDescription": "Allow websites you choose to know your location.",
        # Sharing a screen with its sound may tap system audio (macOS 14.2+).
        "NSAudioCaptureUsageDescription": "Allow websites you choose to hear your Mac's sound when you share your screen.",
        # Chromium checks Bluetooth during cross-device passkey authentication.
        # macOS terminates the process if this purpose string is missing.
        "NSBluetoothAlwaysUsageDescription": "Use Bluetooth for passkeys on nearby devices and to connect to Bluetooth devices you choose on websites.",
        "CFBundleURLTypes": [{
            "CFBundleURLName": args.identifier + ".web",
            "CFBundleTypeRole": "Viewer",
            "LSHandlerRank": "Alternate",
            "CFBundleURLSchemes": ["http", "https"],
        }],
        "CFBundleDocumentTypes": document_types(),
    }
    if has_icon:
        info["CFBundleIconFile"] = Path(APP_ICON).stem
    write_plist(contents / "Info.plist", info)
    helpers = []
    for suffix in ["", " (GPU)", " (Renderer)", " (Plugin)", " (Alerts)"]:
        # CEF finds its helpers by the executable's name, "mapbrowser".
        name = "mapbrowser Helper" + suffix
        helper = frameworks / (name + ".app")
        helper_bin = helper / "Contents" / "MacOS" / name
        helper_bin.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(profile / "mapbrowser-helper", helper_bin)
        kind = suffix.strip().strip("()").lower()
        alerts = kind == "alerts"
        helper_info = dict(info)
        helper_info.update({
            "CFBundleName": name,
            # Web notifications come from the Alerts helper; macOS shows
            # them under its display name and icon.
            "CFBundleDisplayName": args.name if alerts else name,
            "CFBundleExecutable": name,
            "CFBundleIdentifier": args.identifier + ".helper" + ("." + kind if kind else ""),
            "LSUIElement": True,
        })
        helper_info.pop("NSPrincipalClass")
        helper_info.pop("CFBundleURLTypes")
        helper_info.pop("CFBundleDocumentTypes")
        if alerts and has_icon:
            (helper / "Contents" / "Resources").mkdir(parents=True, exist_ok=True)
            shutil.copy2(contents / "Resources" / APP_ICON, helper / "Contents" / "Resources" / APP_ICON)
        else:
            helper_info.pop("CFBundleIconFile", None)
        write_plist(helper / "Contents" / "Info.plist", helper_info)
        helpers.append(helper)
    main_entitlements, helper_entitlements = prepare_signing_artifacts(app, helpers, signing)
    for command in signing_commands(app, destination, helpers, signing, main_entitlements, helper_entitlements):
        run(*command)
    print(app)


if __name__ == "__main__":
    main()
