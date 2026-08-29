#!/usr/bin/env python3
"""Static fail-closed invariants for the native folder transaction boundary."""

from __future__ import annotations

import sys
from pathlib import Path


def fail(message: str) -> None:
    raise SystemExit(f"check_folder_module.py: {message}")


def ordered(text: str, markers: list[str], label: str) -> None:
    positions = [text.find(marker) for marker in markers]
    if any(position < 0 for position in positions) or positions != sorted(positions):
        fail(f"{label} markers are missing or out of order: {markers}")


def main() -> None:
    if len(sys.argv) != 2:
        fail("usage: check_folder_module.py <supernote-poc-root>")
    root = Path(sys.argv[1]).resolve()
    module = (root / "native" / "InkBridgeFolderModule.kt.template").read_text(
        encoding="utf-8"
    )
    native_viewport = (
        root / "native" / "InkBridgeNativeViewport.kt.template"
    ).read_text(encoding="utf-8")
    viewport_core = (
        root / "overlay" / "nativeViewportProviderCore.js"
    ).read_text(encoding="utf-8")
    installer = (root / "scripts" / "install_native.py").read_text(
        encoding="utf-8"
    )
    javascript = (root / "overlay" / "folderCompanionCore.js").read_text(
        encoding="utf-8"
    )
    companion = (root / "overlay" / "folderCompanion.js").read_text(encoding="utf-8")
    app = (root / "overlay" / "App.js").read_text(encoding="utf-8")
    index = (root / "overlay" / "index.js").read_text(encoding="utf-8")
    build_script = (root / "build.sh").read_text(encoding="utf-8")

    if "MSYS2_ARG_CONV_EXCL='/icon.png;/app.npk'" not in build_script:
        fail("Git Bash builds must preserve root-relative plugin payload paths")

    ordered(
        module,
        ["output.fd.sync()", "Os.rename(", "syncDirectory(destination.parentFile!!)"] ,
        "atomic publication",
    )
    ordered(
        javascript,
        ["if (validate) await validate", "const applied = await apply", "const acknowledged = await acknowledge"],
        "apply-before-acknowledge",
    )
    ordered(
        companion,
        ["getDocumentIdentity(", "const collected = representation", "publishPageExport("],
        "hash-before-collect-before-publish",
    )
    ordered(
        module[module.find("fun loadNextManifest") : module.find("fun acknowledgeManifest")],
        ["conflictMessage(context)", "transportConflictStatus(context)", "pendingManifests(context)"],
        "conflict-before-delivery",
    )
    ordered(
        module[module.find("fun publishPageExport") : module.find("fun loadNextManifest")],
        ["context.documentId == expectedDocumentId", "pendingManifests(context)", "atomicWrite(destination"],
        "reject-pending-before-export",
    )
    acknowledgement = module[
        module.find("fun acknowledgeManifest") : module.find("fun recordManifestFailure")
    ]
    ordered(
        acknowledgement,
        ["pendingManifests(context)", "val responseJson", "atomicWrite("],
        "acknowledgement-preflight-before-commit",
    )
    committed_acknowledgement = acknowledgement[acknowledgement.find("atomicWrite(") :]
    for forbidden in ("pendingManifests(", "statusJson(", "recordNativeFailure("):
        if forbidden in committed_acknowledgement:
            fail(f"acknowledgement must not perform fallible {forbidden} work after commit")
    for required in (
        'it.name.endsWith(".operations.json")',
        "recordManifestFailure",
        'return "Edits from both devices were preserved; automatic sync is paused."',
        "Executors.newSingleThreadExecutor",
        "FileInputStream(pdf).use",
        'payload.put("documentId", context.documentId)',
        'checkpointHashes(value, "supernoteAcceptedContentSha256")',
        "recordNativeFailure(",
        "reconcileFailureRecords(context, liveDeliveryIds)",
        "regularFiles(context.incoming).sortedBy { it.name }",
        'value.optInt("schemaVersion", -1) == 1',
        "fix or remove it before newer revisions can be applied",
        "every user-initiated folder action rehashes",
        "isValidAcknowledgement(context, deliveryId)",
        "does not match its delivery",
        "pending InkBridge update(s) before exporting this page",
        "fun validateDocumentIdentity(",
        "context.documentId == expectedDocumentId",
        'payload.put("basedOn", exportRevisionFrontier(context).toJson())',
        'acknowledgement.put("sourceRevisions", revisions.toJson())',
        "wait for the missing predecessor delivery",
        "checkpoint.boox <= applied.boox",
        "fun getDocumentIdentity(",
        "The PDF bytes changed while this page's native ink was collected",
        "Transport checkpoint ${file.name} is incomplete or invalid",
        "validateRepresentation(pdf, it)",
        "Virtual Spread PDF bytes changed after verification",
        "Virtual Spread sidecar bytes changed after verification",
        "inkBridgeDirectory.parentFile?.canonicalFile == sharedStorageRoot",
        'sidecarJson.optString("schema") == "techrebbe.supernote.virtual-spread/v3"',
        'File(context.outgoing, "spread-pages-$suffix.json")',
        '"page-%04d.json".format(Locale.ROOT, representedPages.single() + 1)',
        '"%04d".format(Locale.ROOT, it + 1)',
        "acceptedSupernoteSourceViewHashes(context)",
        'checkpointHashes(checkpoint, "supernoteAcceptedContentSha256")',
        'checkpointHashes(value, "supernoteAcceptedSourceViewSha256")',
        "val overlappingExports = outgoingPageFiles(context)",
        "outgoingExportPages(existing).any(representedPageSet::contains)",
        "existingHash != sourceViewHash",
        "allow it to finish before switching document representations",
        "Could not retire superseded native export",
        "fun getNativeViewport(",
        "nativeViewportReader.read(",
    ):
        if required not in module:
            fail(f"native module is missing required invariant: {required}")
    manifest_scan = module[
        module.find("private fun pendingManifests") : module.find("private fun conflictMessage")
    ]
    for forbidden in ("file.delete()", "context.incoming.delete", "deleteRecursively"):
        if forbidden in manifest_scan:
            fail("incoming-manifest scan must never delete source deliveries")
    if "getSharedPreferences" in module or "cachedIdentity" in module:
        fail("stable document identity must not trust a metadata-only PDF hash cache")
    if "acknowledgementFile(context, deliveryId).isFile) continue" in module:
        fail("incoming deliveries must not trust acknowledgement existence without validation")
    for required in (
        "if (EMBEDDED_MANIFEST)",
        "name: 'Apply Embedded Test'",
        "applyEmbeddedManifest()",
    ):
        if required not in index:
            fail(f"embedded-manifest regression action is missing: {required}")
    for required in (
        "collectCurrentVirtualSpread(",
        "validateDocumentIdentity before identity persistence",
        "return applyVirtualSpreadManifest(",
        "fixtureNativeDescriptor(representation)",
        "await currentNativeViewport(",
        "requireSameNativeViewport(",
        "nativeViewportMap(nativeViewport)",
    ):
        if required not in companion:
            fail(f"Virtual Spread folder integration is missing: {required}")
    for required in (
        'cp "$ROOT/overlay/virtualSpreadAdapterCore.js"',
        'cp "$ROOT/overlay/nativeViewportProviderCore.js"',
        'cp "$ROOT/overlay/virtualSpreadFixture.js"',
    ):
        if required not in build_script:
            fail(f"Virtual Spread package input is missing: {required}")
    ordered(
        app,
        [
            "collectCurrentVirtualSpread(",
            "requireSameDocumentPath(expectedFilePath, filePath)",
            "getCurrentFilePath before identity persistence",
            "await revalidateDocumentIdentity()",
            "getCurrentFilePath after identity validation",
            "persist InkBridge stroke identities",
        ],
        "Virtual Spread identity persistence document revalidation",
    )
    for required in (
        '"com.techrebbe.supernote.virtualspread.viewport"',
        '"com.techrebbe.supernote.virtualspread"',
        '"a5a8551131de84d41660a3cf22d224f320f7a2f05a380282f76f6fe731807c67"',
        "resolveContentProvider(",
        "PackageManager.GET_SIGNING_CERTIFICATES",
        "signingInfo?.apkContentsSigners",
        "response.keySet() == RESPONSE_KEYS",
        "descriptorJson == descriptor.canonicalJson()",
        "sha256(descriptorJson) == descriptorSha256",
        'requireString(response, "documentPath") == documentPath',
        'requireString(response, "sidecarPath") == "$documentPath.json"',
        "requireStable(affine)",
    ):
        if required not in native_viewport:
            fail(f"native viewport consumer is missing required invariant: {required}")
    for required in (
        "InkBridgeNativeViewport.kt.template",
        "com.techrebbe.supernote.virtualspread.viewport",
    ):
        if required not in installer:
            fail(f"native viewport packaging is missing required input: {required}")
    for required in (
        "requireNativeViewportResult",
        "requireSameNativeViewport",
        "nativeViewportForVirtualSpread(",
        "expected.pageLoadGeneration !== current.pageLoadGeneration",
        "expected.snapshotId !== current.snapshotId",
    ):
        if required not in viewport_core:
            fail(f"native viewport JavaScript boundary is incomplete: {required}")
    print("InkBridge native folder invariants passed")


if __name__ == "__main__":
    main()
