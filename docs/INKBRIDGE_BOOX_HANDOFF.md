# BOOX NeoReader handoff companion

`boox-companion` is a small, BOOX-only Android app that transfers revisioned InkBridge PDF views into and out of NeoReader. It is not a PDF reader and does not replace NeoReader.

## Why the handoff is versioned

Real-device testing established two important NeoReader behaviors:

1. Replacing bytes at an already-open path can display new annotations without adopting them as editable NeoReader ink.
2. Keeping a stable copy and a revisioned copy visible can cause NeoReader to embed edits into the wrong file.

The companion therefore gives every broker output a fresh filename and keeps exactly one active view for a logical document. Old broker views move into a hidden `.retired` directory.

## Device layout

The local root is `/storage/emulated/0/Documents/InkBridge`:

```text
InkBridge/
  inkbridge-doc-v1-<original-pdf-sha256>/
    incoming/
      delivery.pdf
      delivery.inkbridge.json
    active/
      Original__ib-b2-s4-g19.pdf
    .retired/
      Original__ib-b1-s4-g18.pdf
    outgoing/
      Original__ib-b2-s4-g19__boox-finalized-g1-<hash>.pdf
      Original__ib-b2-s4-g19__boox-finalized-g1-<hash>.pdf.inkbridge.json
    .inkbridge-state.json
    .inkbridge-install.json  # present only while an install is being committed/recovered
```

The stable document ID is the broker's `inkbridge-doc-v1-<SHA-256 of immutable original PDF>`. Filenames never define identity.

## Broker delivery descriptor

The folder transport places a create-only PDF and JSON descriptor together in `incoming/`, publishing the descriptor last:

```json
{
  "schemaVersion": 1,
  "producer": "inkbridge-broker",
  "eventId": "broker-event-123",
  "documentId": "inkbridge-doc-v1-<64 lowercase hex characters>",
  "originalFileName": "Example.pdf",
  "sourceRevisions": { "boox": 2, "supernote": 4 },
  "sourceGeneration": 19,
  "contentSha256": "<PDF SHA-256>",
  "pdfFileName": "broker-b00000000000000000002-s00000000000000000004-g00000000000000000019-<hash>.pdf"
}
```

The companion validates the producer, document ID, filenames, source generation, content hash, and revision frontier before installing anything.

## Safety rules

- Duplicate events are idempotent and cannot create a second active PDF.
- Incoming descriptors whose revisions are already dominated by the active frontier are ignored even after their event IDs age out of the bounded replay cache, so expired files cannot block a newer delivery.
- Malformed descriptors and descriptor/PDF pairs that are not complete yet are skipped, allowing later valid deliveries to remain installable.
- A stale or incomparable revision is rejected; there is no latest-file-wins behavior.
- A same-revision PDF with different bytes is rejected.
- If NeoReader changed the active PDF, a new broker view is refused until those changes are finalized.
- Even after finalization, the old view is retained until a new broker delivery advances the BOOX revision, proving that the broker accepted the finalized BOOX edit.
- Incoming and outgoing PDFs are streamed, so a 500 MB PDF is never loaded wholly into memory.
- Hashing, copying, synchronization, state recovery, and finalization run on one serialized background worker. The activity disables actions while work is running and performs only status updates and the final NeoReader launch on Android's main thread.
- Files and state use synchronized temporary files plus create-only publication. Existing destination bytes are never overwritten, and each streamed PDF copy is hash-verified before its temporary file is published.
- A durable install intent keeps the previous active PDF in place until the replacement PDF and state are committed. After interruption or power loss, the next companion action completes the install or safely discards an unpublished attempt before retiring the predecessor.
- If either member of an already-finalized outgoing PDF/descriptor pair disappears, the next finalize action deterministically reconstructs the missing artifact from the unchanged active PDF and saved revision state.

## User flow

1. **Install next update** validates and installs the next broker delivery at a fresh active path.
2. **Open active document in NeoReader** sends that exact file path to BOOX NeoReader.
3. After editing and using NeoReader's **Embed Data to PDF**, return to the companion.
4. **Finalize BOOX changes** creates an immutable outgoing PDF and a broker `StorageEvent` sidecar.
5. The folder transport validates both files. At the current frontier it uploads compact operations; if the finalized view is stale or concurrent, it uploads the full PDF with its original `basedOn` revisions as conflict evidence. The broker processes the event conditionally and eventually returns a newer broker delivery.

The outgoing event is based on the active revision pair and advances the BOOX source revision by one. Its deterministic event ID makes repeated finalization idempotent.

## Folder-transport integration

Set `booxHandoffRoot` in the folder-transport configuration to the local mirror of `/storage/emulated/0/Documents/InkBridge`. The folder transport downloads broker-generated BOOX views into the matching stable-document `incoming` directory and scans finalized companion artifacts from `outgoing`. The Android app itself has no cloud credentials and performs no background upload.

This milestone does not add cloud resources or change deployed broker infrastructure. Folder mirroring between the computer and BOOX remains an adapter/setup concern; the descriptor, identity, revision, and create-only publication rules do not depend on the mirroring tool.

## ADB test actions

The debug build exposes three explicit actions. Their intent filters exist only in the debug manifest, and release builds ignore explicit automation actions even if another app targets the launcher activity directly. Always select the physical BOOX serial when more than one Android device is connected:

```powershell
adb -s <BOOX_SERIAL> shell am start -a dev.inkbridge.boox.action.INSTALL_NEXT
adb -s <BOOX_SERIAL> shell am start -a dev.inkbridge.boox.action.OPEN_ACTIVE
adb -s <BOOX_SERIAL> shell am start -a dev.inkbridge.boox.action.FINALIZE_ACTIVE
```

Relevant logs use the `INKBRIDGE_BOOX_HANDOFF` tag.
