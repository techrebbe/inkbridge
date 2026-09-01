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
    manifest_progress = (
        root / "native" / "InkBridgeManifestProgress.kt.template"
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
    manifest_apply = (root / "overlay" / "manifestApply.js").read_text(
        encoding="utf-8"
    )
    identity_ledger = (root / "overlay" / "identityLedgerCore.js").read_text(
        encoding="utf-8"
    )
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
        [
            "context.documentId == expectedDocumentId",
            "pendingManifests(context)",
            "existingHash != sourceViewHash",
            "atomicWrite(destination",
            "identityLedgerFile(context)",
        ],
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
        "fun recordVirtualSpreadStepApplied(",
        "manifestProgress.record(",
        "fun loadIdentityState(",
        "loadIdentityLedger(context)",
        'File(context.directory, IDENTITY_LEDGER_FILE)',
        "Pending native export ${file.name} targets another document",
        "validateIdentityLedger(",
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
        "const applied = await applyVirtualSpreadManifest(",
        "fixtureNativeDescriptor(representation)",
        "await currentNativeViewport(",
        "requireSameNativeViewport(",
        "nativeViewportMap(nativeViewport)",
        "planVirtualSpreadDelivery(",
        "recordVirtualSpreadStepApplied(",
    ):
        if required not in companion:
            fail(f"Virtual Spread folder integration is missing: {required}")
    for required in (
        'cp "$ROOT/overlay/virtualSpreadAdapterCore.js"',
        'cp "$ROOT/overlay/nativeViewportProviderCore.js"',
        'cp "$ROOT/overlay/virtualSpreadFixture.js"',
        'cp "$ROOT/overlay/emrPointSpaceCore.js"',
        'cp "$ROOT/overlay/identityLedgerCore.js"',
    ):
        if required not in build_script:
            fail(f"Virtual Spread package input is missing: {required}")
    virtual_spread_collection = app[
        app.find("collectCurrentVirtualSpread(") : app.find("export async function exportCurrentSupernotePage")
    ]
    if "PluginFileAPI.modifyElements(" in virtual_spread_collection:
        fail("Virtual Spread export must not rewrite native strokes merely to persist identity metadata")
    for required in (
        "reconcileStableStrokeIdentities(",
        "translatedShapeMatches(",
        "Stable identity reconciliation is ambiguous",
        "delete rewritten.nativeElementUuid",
    ):
        if required not in identity_ledger:
            fail(f"durable non-mutating identity reconciliation is missing: {required}")
    if "normalizedEmrPoint(point, source)" not in app:
        fail("Virtual Spread export must use each native stroke's authoritative EMR range")
    for required in (
        "if (useElementEmrRange)",
        "PointUtils.emrPoint2Android(point, pageSize)",
        "useElementEmrRange: true",
    ):
        if required not in app:
            fail(f"ordinary and Virtual Spread export conversion modes are not separated: {required}")
    if "PluginCommAPI.getPageDisplaySize()" in app:
        fail("plugin-preview firmware does not expose getPageDisplaySize")
    for required in (
        "requireEmrRangeForInsertion(emrRange)",
        "useElementEmrRange",
        "PointUtils.emrPoint2Android(point, pageSize)",
    ):
        if required not in manifest_apply:
            fail(f"Virtual Spread insertion EMR guard is missing: {required}")
    for required in (
        "stable identity fallback",
        "representation.documentId",
        "buildVirtualSpreadSnapshot(",
    ):
        if required not in virtual_spread_collection:
            fail(f"non-mutating Virtual Spread identity fallback is missing: {required}")
    ordered(
        companion[companion.find(": await collectCurrentSupernotePage(identity.documentId)") :],
        [
            ": await collectCurrentSupernotePage(identity.documentId)",
            "if (representation)",
            "requireSameNativeViewport(",
            "await native.loadIdentityState(",
            "reconcileStableStrokeIdentities(",
            "await native.publishPageExport(",
        ],
        "unconditional post-collection viewport revalidation",
    )
    ordered(
        companion[companion.find("const applied = await applyVirtualSpreadManifest(") :],
        [
            "const applied = await applyVirtualSpreadManifest(",
            "await finishVirtualSpreadStep(",
            "await native.recordVirtualSpreadStepApplied(",
            "await PluginCommAPI.reloadFile()",
        ],
        "post-apply viewport fence before durable progress and redraw",
    )
    ordered(
        companion[companion.find("if (plan.complete)") :],
        [
            "if (plan.complete)",
            "await PluginCommAPI.reloadFile()",
            "return completedVirtualSpreadDelivery(",
        ],
        "completed-delivery redraw retry before acknowledgement",
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
        "InkBridgeManifestProgress.kt.template",
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
        "requireVirtualSpreadProgress",
        "completedVirtualSpreadDelivery",
        "requireSameNativeViewport(expectedViewport, await readCurrentViewport())",
    ):
        if required not in viewport_core:
            fail(f"native viewport JavaScript boundary is incomplete: {required}")
    for required in (
        "completedStepIds",
        "Virtual Spread step progress changed after it was committed",
        "Os.rename(",
        "output.fd.sync()",
    ):
        if required not in manifest_progress:
            fail(f"Virtual Spread manifest progress is incomplete: {required}")
    print("InkBridge native folder invariants passed")


if __name__ == "__main__":
    main()
