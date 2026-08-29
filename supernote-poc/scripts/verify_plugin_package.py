#!/usr/bin/env python3
"""Verify the .snplg contains the reviewed JS bundle and native folder APK."""

from __future__ import annotations

import io
import json
import re
import sys
import zipfile
from pathlib import Path


EXPECTED_REACT_PACKAGES = ["com.inkbridgepoc.InkBridgeFolderPackage"]
EXPECTED_NATIVE_CLASSES = (
    b"InkBridgeFolderModule",
    b"InkBridgeFolderPackage",
    b"InkBridgeNativeViewport",
)
MINIMUM_NATIVE_APK_SIZE = 1_000_000


def fail(message: str) -> None:
    raise SystemExit(f"verify_plugin_package.py: {message}")


def read_json(data: bytes, label: str) -> dict[str, object]:
    try:
        value = json.loads(data.decode("utf-8-sig"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{label} is not valid UTF-8 JSON: {error}")
    if not isinstance(value, dict):
        fail(f"{label} must contain an object")
    return value


def one(archive: zipfile.ZipFile, name: str) -> bytes:
    entries = [entry for entry in archive.infolist() if entry.filename == name]
    if len(entries) != 1 or entries[0].is_dir():
        fail(f"expected one file named {name}, found {len(entries)}")
    return archive.read(entries[0])


def verify_native_apk(data: bytes) -> None:
    if len(data) < MINIMUM_NATIVE_APK_SIZE:
        fail(f"embedded app.npk is implausibly small: {len(data)} bytes")
    try:
        with zipfile.ZipFile(io.BytesIO(data)) as native:
            corrupt = native.testzip()
            if corrupt:
                fail(f"embedded app.npk has a corrupt entry: {corrupt}")
            dex_names = sorted(
                name
                for name in native.namelist()
                if re.fullmatch(r"classes(?:[2-9]|[1-9][0-9]+)?\.dex", name)
            )
            if not dex_names:
                fail("embedded app.npk contains no Android bytecode")
            dex = [native.read(name) for name in dex_names]
            for class_name in EXPECTED_NATIVE_CLASSES:
                if not any(class_name in payload for payload in dex):
                    fail(f"embedded app.npk is missing {class_name.decode()}")
    except zipfile.BadZipFile as error:
        fail(f"embedded app.npk is not a valid APK: {error}")


def verify(package_path: Path, root: Path) -> None:
    source_config = read_json(
        (root / "PluginConfig.json").read_bytes(), "source PluginConfig.json"
    )
    plugin_key = source_config.get("pluginKey")
    if not isinstance(plugin_key, str) or not plugin_key:
        fail("source PluginConfig.json has no pluginKey")
    bundle_name = f"{plugin_key}.bundle"
    expected_files = {
        "PluginConfig.json",
        "app.npk",
        "icon.png",
        bundle_name,
        "drawable-mdpi/assets_icon.png",
    }
    try:
        with zipfile.ZipFile(package_path) as package:
            corrupt = package.testzip()
            if corrupt:
                fail(f"plugin package has a corrupt entry: {corrupt}")
            names = [entry.filename for entry in package.infolist() if not entry.is_dir()]
            if len(names) != len(set(names)):
                fail("plugin package contains duplicate entries")
            if set(names) != expected_files:
                fail(
                    "plugin payload differs from reviewed layout: "
                    f"missing={sorted(expected_files - set(names))}, "
                    f"unexpected={sorted(set(names) - expected_files)}"
                )
            packaged = read_json(one(package, "PluginConfig.json"), "packaged config")
            expected = dict(source_config)
            expected["iconPath"] = "/icon.png"
            expected["reactPackages"] = EXPECTED_REACT_PACKAGES
            expected["nativeCodePackage"] = "/app.npk"
            if packaged != expected:
                fail("packaged PluginConfig.json does not publish the reviewed native package")
            bundle = one(package, bundle_name)
            for marker in (
                b"INKBRIDGE_FOLDER_DONE",
                b"publishPageExport",
                b"getNativeViewport",
                b"rtl-reader-native-viewport-v1",
            ):
                if marker not in bundle:
                    fail(f"JavaScript bundle is missing {marker.decode()}")
            verify_native_apk(one(package, "app.npk"))
    except zipfile.BadZipFile as error:
        fail(f"plugin package is not a valid ZIP: {error}")


def main() -> None:
    if len(sys.argv) != 3:
        fail("usage: verify_plugin_package.py <package.snplg> <supernote-poc-root>")
    verify(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
    print(f"Verified native InkBridge package: {sys.argv[1]}")


if __name__ == "__main__":
    main()
