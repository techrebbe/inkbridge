# BOOX NeoReader handoff companion

`boox-companion` is a small, BOOX-only Android app that transfers revisioned InkBridge PDF views into and out of NeoReader. It is not a PDF reader and does not replace NeoReader.

## Why the handoff is versioned

Real-device testing established two important NeoReader behaviors:

1. Replacing bytes at an already-open path can display new annotations without adopting them as editable NeoReader ink.
2. Keeping a stable copy and a revisioned copy visible can cause NeoReader to embed edits into the wrong file.

The companion therefore gives every broker output a fresh filename and keeps exactly one active view for a logical document. The immediate predecessor moves into a hidden `.retired` directory while NeoReader releases it; after the companion has paused for NeoReader and then resumed from that foreground round trip, the next install safely compacts the older predecessor so retained full-PDF storage remains bounded.

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
      boox-g1-<active-pdf-hash>.operations.json
      boox-g1-<active-pdf-hash>.operations.json.inkbridge.json
      # A full finalized PDF pair appears only when native conversion cannot run safely.
    .inkbridge-baseline-<broker-pdf-hash>.json
    .inkbridge-state.json
    .inkbridge-installed.json # durable current broker-view acknowledgement
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
- After an install commits, the companion atomically publishes `.inkbridge-installed.json`. The transport uses this acknowledgement to keep the current incoming PDF/descriptor as the active recovery pair while durably retiring older dominated pairs. Losing the transport checkpoint therefore cannot recreate obsolete 300–500 MB deliveries. Before trusting it, the transport matches the full event/generation/revision/hash identity against live broker metadata and persists one bounded verified-install receipt, including the broker object path, generation, and size. That receipt remains authoritative when the broker later replaces its stable cloud path with a newer generation. If the acknowledged local pair disappears, the transport downloads that exact historical Cloud Storage generation, verifies its hash, and republishes the descriptor last before processing newer deliveries. This recovery relies on the deployment's documented Cloud Storage object versioning and retention policy.
- Incoming descriptors whose revisions are already dominated by the active frontier are ignored even after their event IDs age out of the bounded replay cache. The cache retains 64 recent byte-bounded IDs so two worst-case handoff states still fit in one crash-recovery intent.
- Malformed descriptors and descriptor/PDF pairs that are not complete yet are skipped, allowing later valid deliveries to remain installable.
- A stale or incomparable revision is rejected; there is no latest-file-wins behavior.
- A storage-generation republish of the already installed revision and content is ignored before download and any equivalent staged pair is retired; a same-revision PDF with different bytes is rejected as conflicting content.
- If NeoReader changed the active PDF, a new broker view is refused until those changes are finalized.
- Even after finalization, the old view is retained until a new broker delivery advances the BOOX revision, proving that the broker accepted the finalized BOOX edit.
- Incoming PDFs and rare full-PDF fallback snapshots are streamed. Normal BOOX finalization parses the active PDF locally and publishes only compact operation JSON, so a 300-500 MB PDF is not mirrored or uploaded for each ink edit.
- Hashing, copying, synchronization, state recovery, and finalization run on one serialized background worker. The activity disables actions while work is running and performs only status updates and the final NeoReader launch on Android's main thread.
- Files and state use synchronized temporary files plus create-only publication. Existing destination bytes are never overwritten, and each streamed PDF copy is hash-verified before its temporary file is published.
- A durable install intent keeps the previous active PDF in place until the replacement PDF and state are committed. After interruption or power loss, the next companion action completes the install or safely discards an unpublished attempt before retiring the predecessor.
- Returning to the companion after it paused for a successfully dispatched NeoReader launch records the versioned-path handoff boundary; merely preparing or dispatching the asynchronous Android intent does not. The pending target is persisted before dispatch, but a pause can be recorded only when the same in-memory tracker was armed after `startActivity` returned successfully. A process restart therefore cannot turn an unrelated permission/Home pause into a false confirmation. Once that armed pause is durably observed, the pending marker survives activity/process recreation and is cleared only after the handoff-state confirmation commits. InkBridge retains and watches at most one full predecessor PDF; the next confirmed install rechecks late bytes, publishes any final edit, and crash-safely removes the older predecessor. If the active handoff was never opened, a further install is refused instead of deleting uncertain data or growing storage without a bound.
- Compact output is published payload-first and descriptor-last with deterministic names and hashes. A retry verifies identical existing bytes; incomplete publication never overwrites another artifact. The manual full-PDF fallback retains its durable finalize-intent recovery.
- After the broker accepts a finalized BOOX revision and the companion installs a broker view containing that revision, the transport writes a synchronized retirement marker and removes the acknowledged outgoing PDF/descriptor pair. An interrupted cleanup resumes from the marker, so normal finalizations do not accumulate full-document snapshots.

## User flow

1. **Install next update** validates a fresh broker view, prepares a compact stroke baseline locally, and opens that exact versioned path in NeoReader.
2. Write normally in NeoReader and exit back to InkBridge.
3. On resume, the companion confirms the handoff and automatically diffs the closed PDF against the saved baseline through the same Rust converter used on desktop.
4. The companion publishes a small `boox_operation_manifest` plus its `StorageEvent` descriptor. Repeated resume/event delivery is idempotent and cannot duplicate strokes.
5. The folder transport uploads that manifest directly; it does not read or transfer the active PDF. A stale manifest keeps its original `basedOn` frontier so the broker preserves it as conflict evidence instead of rebasing or choosing latest-file-wins.
6. If NeoReader did not embed its live ink at close, or the native parser encounters corruption outside the narrow Android-safe trailing-xref repair, InkBridge reports that compact sync is unavailable. The existing **Embed Data to PDF** and **Finalize BOOX changes** full-PDF path remains the explicit recovery route.

## Note Air 4C compact-handoff hardware result

The companion's versioned local path passed a two-cycle test on a 210 MiB PDF on August 25, 2026:

- The first normal NeoReader close, without manual **Embed Data to PDF** or **Finalize BOOX changes**, produced 7 compact operations in a 136,750-byte manifest. No full PDF was placed in the outgoing folder.
- The broker accepted BOOX revision 1 and rebuilt the immutable-original-derived BOOX view. NeoReader adopted that fresh versioned path as editable ink.
- The imported handwriting was moved with lasso and one character was erased, then the document was closed normally again.
- NeoReader's second rewrite contained a malformed trailing xref-stream `/Length`. The Android-safe in-memory repair recovered the exact stream boundary, after which the converter emitted 8 operations (6 upserts and 2 deletions) in 235,408 bytes. The device manifest was byte-identical to host recovery output, and no full PDF was published.
- Replaying both device manifests sequentially through one broker state advanced cleanly to BOOX revision 2 with 6 active strokes, 2 tombstones, and both event IDs recorded. The broker rejected neither valid edit and created no duplicate stroke.

The replay also exposed that broker-generated standard PDF `/Ink` is a lossy view of native pen metadata. The broker now requires stable identity, page, visible geometry, width, and grayscale to match the canonical precondition while restoring native-only pen type, layer, origin, and pressure when the visible style was unchanged. A real geometry mismatch is still rejected as stale input.

This validates the 210 MiB case only; it does not claim that every 300-500 MB document or every malformed-PDF variant has passed hardware testing. Broader corruption still uses the explicit full-PDF recovery path.

## Folder-transport integration

Set `booxHandoffRoot` in the folder-transport configuration to the local mirror of `/storage/emulated/0/Documents/InkBridge`. The folder transport downloads broker-generated BOOX views into the matching stable-document `incoming` directory and scans finalized companion artifacts from `outgoing`. The Android app itself has no cloud credentials and performs no background upload.

This milestone does not add cloud resources or change deployed broker infrastructure. Folder mirroring between the computer and BOOX remains an adapter/setup concern; the descriptor, identity, revision, and create-only publication rules do not depend on the mirroring tool.

## Integrated Google Drive experiment result

The Note Air 4C test produced a clear split:

- For the small disposable PDF, normal NeoReader close embedded live `#ONYX-STROKE`/`onyxpoints` data into the local cached PDF and uploaded the complete changed PDF to Drive without using **Embed Data to PDF** manually.
- A broker-generated replacement opened at a fresh path was adopted as editable NeoReader state; lasso, move, and erase all worked.
- An externally replaced Drive revision was not shown merely by reopening the stale BOOX copy, so Drive cannot be the authoritative inbound handoff.
- A 210 MiB source embedded locally, but BOOX did not submit the updated file to Drive. This matches BOOX's documented 200 MB source-file limit for reading-data synchronization and makes whole-PDF cloud upload unsuitable for the user's 300-500 MB documents.

Integrated Drive remains an optional convenience for small documents, not the correctness boundary. The production direction is therefore: versioned companion input, local NeoReader close/embed, on-device Rust conversion, and compact-manifest folder transport. The broker remains canonical; Drive modification time and "latest file" ordering never determine state.

Official background:

- [BOOX integrated third-party cloud storage](https://shop.boox.com/en-ca/blogs/news/new-feature-integrated-third-party-cloud-storage)
- [BOOX reading-data syncing](https://help.boox.com/hc/en-us/articles/10701453841044-Reading-Data-Syncing)

## ADB test actions

The debug build exposes three explicit actions. Their intent filters exist only in the debug manifest, and release builds ignore explicit automation actions even if another app targets the launcher activity directly. Always select the physical BOOX serial when more than one Android device is connected:

```powershell
adb -s <BOOX_SERIAL> shell am start -a dev.inkbridge.boox.action.INSTALL_NEXT
adb -s <BOOX_SERIAL> shell am start -a dev.inkbridge.boox.action.OPEN_ACTIVE
adb -s <BOOX_SERIAL> shell am start -a dev.inkbridge.boox.action.FINALIZE_ACTIVE
```

Relevant logs use the `INKBRIDGE_BOOX_HANDOFF` tag.
