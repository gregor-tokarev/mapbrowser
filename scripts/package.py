#!/usr/bin/env python3
"""Package a release: the app as a disk image for people and a zip for updates, then the update manifest."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
NAME = "Map Browser"
# By the names people know, and by Rust's `std::env::consts::ARCH`, which
# the app looks itself up by in the manifest.
ARCHES = {"arm64": "aarch64", "x64": "x86_64"}


def asset_name(arch, extension):
    # Without the version, so releases/latest/download/<name> always links
    # to the newest one.
    return f"Map-Browser-mac-{arch}.{extension}"


def run(*args):
    print("+", " ".join(map(str, args)), flush=True)
    subprocess.run(args, check=True)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as file:
        for chunk in iter(lambda: file.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def package_version():
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version=1"], cwd=ROOT))
    return next(package["version"] for package in metadata["packages"] if package["name"] == "mapbrowser")


def disk_image(app, path):
    """A disk image with the app beside a link to Applications, to drag it onto."""
    with tempfile.TemporaryDirectory() as folder:
        folder = Path(folder)
        run("ditto", app, folder / app.name)
        (folder / "Applications").symlink_to("/Applications")
        # hdiutil now and then finds the new image busy; a moment later it isn't.
        for attempt in range(3):
            try:
                run("hdiutil", "create", "-volname", NAME, "-srcfolder", folder, "-fs", "HFS+", "-format", "UDZO", "-ov", path)
                return
            except subprocess.CalledProcessError:
                if attempt == 2:
                    raise
                time.sleep(10)


def package_app(args):
    app = args.app.resolve()
    if app.suffix != ".app" or not app.is_dir():
        raise SystemExit(f"{app} is not an app bundle.")
    args.out.mkdir(parents=True, exist_ok=True)
    archive = args.out / asset_name(args.arch, "zip")
    archive.unlink(missing_ok=True)
    # ditto keeps the framework's symlinks and the code signatures.
    run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", app, archive)
    disk_image(app, args.out / asset_name(args.arch, "dmg"))
    asset = {"arch": ARCHES[args.arch], "name": archive.name, "size": archive.stat().st_size, "sha256": sha256(archive)}
    (args.out / f"{args.arch}.json").write_text(json.dumps(asset, indent=2) + "\n")


def write_manifest(args):
    macos = {}
    for part in args.assets:
        asset = json.loads(part.read_text())
        macos[asset["arch"]] = {
            "url": f"{args.base_url.rstrip('/')}/{asset['name']}",
            "size": asset["size"],
            "sha256": asset["sha256"],
        }
    if not macos:
        raise SystemExit("No packaged apps to list.")
    json.dump({"version": args.version, "macos": macos}, sys.stdout, indent=2)
    sys.stdout.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    app = commands.add_parser("app", help="Zip the app and make its disk image; write <arch>.json describing the zip")
    app.add_argument("--app", type=Path, required=True)
    app.add_argument("--arch", choices=ARCHES, required=True)
    app.add_argument("--out", type=Path, default=ROOT / "dist")
    manifest = commands.add_parser("manifest", help="Print latest.json from the <arch>.json files")
    manifest.add_argument("--version", required=True)
    manifest.add_argument("--base-url", required=True, help="Where the release's files are, such as https://github.com/<repo>/releases/download/v1.2.3")
    manifest.add_argument("assets", type=Path, nargs="+")
    commands.add_parser("version", help="Print the version in Cargo.toml")
    args = parser.parse_args()
    if args.command == "app":
        if sys.platform != "darwin":
            raise SystemExit("Packaging the app needs macOS.")
        package_app(args)
    elif args.command == "manifest":
        write_manifest(args)
    else:
        print(package_version())


if __name__ == "__main__":
    main()
