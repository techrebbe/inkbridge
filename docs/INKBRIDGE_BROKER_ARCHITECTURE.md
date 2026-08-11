# InkBridge two-folder broker

This milestone replaces the desktop-runner direction with a storage-independent synchronization
core. It does **not** create or deploy Google Cloud resources, enable APIs, or incur cloud costs.

The closed desktop-runner branch remains a reference for future device adapters and USB/local
hardware testing. Its ADB discovery and device-specific transport findings remain useful, but none
of its desktop coordination or polling logic is carried into this broker core.

## Invariants

- `BOOX_Folder` and `Supernote_Folder` are device-specific generated views of one logical document.
- The immutable original PDF is addressed by its SHA-256-derived document ID, not by a filename.
- Canonical state owns revision history, stroke identity, tombstones, source generations, content
  hashes, processed event IDs, generated-output markers, and conflict records.
- A device event states the exact revision pair on which it was based. If canonical state has moved
  beyond that pair, the incoming bytes are preserved under `Conflicts/` and processing reports a
  conflict. There is no latest-file-wins path.
- Destination writes require the generation observed during planning. The broker also checks that
  the current destination hash is the last hash it generated, so an untracked device edit cannot be
  overwritten merely because it arrived before the broker read the object.
- Broker outputs carry producer, event, document, revision, and content-hash metadata. Finalization
  events for those outputs are acknowledged and ignored, preventing synchronization loops.
- Duplicate event IDs are idempotent and cannot add annotations twice.

## Proposed object layout

```text
Originals/
  <document-id>/original.pdf                    immutable

Canonical/
  <document-id>/state.json                      broker-owned canonical state

BOOX_Folder/
  <document-id>/<logical-name>.pdf              editable PDF /Ink view
  <document-id>/upload-<revision>.pdf            finalized NeoReader input

Supernote_Folder/
  <document-id>/incoming/<event-id>.operations.json
                                                ordered native operation manifests
  <document-id>/export-<revision>.json           finalized native stroke export

Conflicts/
  <document-id>/<event-id>/incoming.<ext>        preserved concurrent input
```

The folder adapters may expose friendlier matching filenames to users, but they must attach the
stable `documentId`, `sourceRevision`, `basedOn`, content hash, and generation to the event envelope.
Renaming either device view therefore does not create a new logical document.

## Event envelope

`StorageEvent` is deliberately provider-neutral:

```json
{
  "schemaVersion": 1,
  "eventId": "cloud-event-id",
  "documentId": "inkbridge-doc-v1-<original-sha256>",
  "source": "boox",
  "objectPath": "BOOX_Folder/<document-id>/upload-2.pdf",
  "sourceGeneration": 418,
  "sourceRevision": 2,
  "basedOn": { "boox": 1, "supernote": 1 },
  "contentSha256": "<sha256>"
}
```

`brokerOutput` is present only when an adapter converts broker metadata back into an event. The
broker verifies it against object metadata before treating the event as a loop.

## Processing directions

### BOOX to Supernote

The broker runs the existing `inkbridge-convert` NeoReader parser against the finalized PDF and the
active canonical baseline. Standard `/Ink` and NeoReader `#ONYX-STROKE` annotations become a
Supernote-native upsert/delete manifest. The converter's qpdf recovery path remains active for the
malformed incremental xref tables observed on real hardware.

### Supernote to BOOX

The broker reads the native page export, updates stable canonical stroke IDs and tombstones, then
generates a fresh device view from the immutable original. Every active stroke is a separate PDF
`/Ink` annotation with a stable `/NM`, `/InkList`, border style, and `/AP` appearance stream. Rebuilding
from the immutable original prevents annotations from accumulating on each round trip.

## Local validation

```bash
cargo test -p inkbridge-broker
cargo run -p inkbridge-broker -- document-id path/to/original.pdf
docker build --platform linux/amd64 -f Dockerfile.broker -t inkbridge-broker:local .
docker run --rm inkbridge-broker:local --help
```

Private real-device fixtures stay outside the public repository. To repeat the parity gate locally:

```bash
INKBRIDGE_REAL_FIXTURE_ROOT=/path/to/artifacts/dual-device-test \
  cargo test -p inkbridge-broker \
  real_device_manifest_is_byte_identical_to_proven_converter_output -- --ignored
```

That test requires byte-for-byte equality with the already-proven Shapiro Supernote manifest.

## Later Google Cloud deployment (not implemented here)

1. A private Cloud Storage bucket holds the layout above with object versioning and retention rules.
2. Eventarc sends finalized-object events for the two device input prefixes to a private Cloud Run
   service built from `Dockerfile.broker`.
3. A Cloud Storage adapter supplies object bytes, metadata, hashes, and generation-match writes.
4. Firestore stores canonical state and processed-event records. A transaction reserves a state
   revision and durable output-outbox item; a worker performs the generation-conditional object
   write and completes the outbox item. This realizes the broker's atomic commit contract without
   pretending Cloud Storage and Firestore share a transaction.
5. Least-privilege service accounts restrict the broker to the private bucket prefixes and its own
   Firestore collection. Authentication, regions, retention, budgets, and alerting are deployment
   decisions for the later infrastructure milestone.

BOOX folder automation, the Supernote companion/plugin transport, and multiwriter operation merging
remain separate later milestones. This core intentionally reports conservative conflicts first.
