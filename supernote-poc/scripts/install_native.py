#!/usr/bin/env python3
"""Install InkBridge's reviewed folder module into a generated RN project."""

from __future__ import annotations

import re
import sys
from pathlib import Path


def fail(message: str) -> None:
    raise SystemExit(f"install_native.py: {message}")


def main() -> None:
    if len(sys.argv) != 3:
        fail("usage: install_native.py <generated-project> <repo-root>")

    project = Path(sys.argv[1]).resolve()
    repo_root = Path(sys.argv[2]).resolve()
    java_root = project / "android" / "app" / "src" / "main" / "java"
    applications = list(java_root.rglob("MainApplication.kt"))
    if len(applications) != 1:
        fail(f"expected one MainApplication.kt, found {len(applications)}")

    application = applications[0]
    text = application.read_text(encoding="utf-8")
    package_match = re.search(r"(?m)^package\s+([A-Za-z0-9_.]+)\s*$", text)
    if not package_match:
        fail("could not determine generated Android package")
    package_name = package_match.group(1)
    package_dir = java_root.joinpath(*package_name.split("."))
    package_dir.mkdir(parents=True, exist_ok=True)

    for source_name in (
        "InkBridgeFolderModule.kt.template",
        "InkBridgeFolderPackage.kt.template",
        "InkBridgeManifestProgress.kt.template",
        "InkBridgeNativeViewport.kt.template",
    ):
        source = repo_root / "native" / source_name
        rendered = source.read_text(encoding="utf-8").replace(
            "__PACKAGE__", package_name
        )
        destination = package_dir / source_name.removesuffix(".template")
        destination.write_text(rendered, encoding="utf-8", newline="\n")

    registration = "add(InkBridgeFolderPackage())"
    if registration not in text:
        marker = "PackageList(this).packages.apply {"
        marker_index = text.find(marker)
        if marker_index < 0:
            fail("could not find PackageList(...).packages.apply block")
        insert_at = text.find("\n", marker_index)
        if insert_at < 0:
            fail("could not find native package insertion point")
        line_start = text.rfind("\n", 0, marker_index) + 1
        indent = re.match(r"\s*", text[line_start:marker_index]).group(0)
        text = (
            text[: insert_at + 1]
            + f"{indent}  {registration}\n"
            + text[insert_at + 1 :]
        )
        application.write_text(text, encoding="utf-8", newline="\n")

    manifest = project / "android" / "app" / "src" / "main" / "AndroidManifest.xml"
    manifest_text = manifest.read_text(encoding="utf-8")
    viewport_authority = "com.techrebbe.supernote.virtualspread.viewport"
    if viewport_authority not in manifest_text:
        application_marker = "    <application"
        marker_index = manifest_text.find(application_marker)
        if marker_index < 0:
            fail("could not find Android application manifest entry")
        query = (
            "    <queries>\n"
            "        <provider\n"
            f'            android:authorities="{viewport_authority}" />\n'
            "    </queries>\n\n"
        )
        manifest_text = (
            manifest_text[:marker_index] + query + manifest_text[marker_index:]
        )
        manifest.write_text(manifest_text, encoding="utf-8", newline="\n")

    print(f"Installed InkBridgeFolderModule in Android package {package_name}")


if __name__ == "__main__":
    main()
