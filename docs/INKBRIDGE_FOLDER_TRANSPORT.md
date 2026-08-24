# InkBridge finalized-folder transport

This milestone connects local BOOX and Supernote folders to the already-deployed private broker.
It replaces manual Cloud Storage uploads and downloads. It does not change the broker, deploy new
resources, or increase the Cloud Run minimum-instance setting.

## Responsibilities

The transport deliberately does less than the broker:

- it waits until a local file has stopped changing before marking it sync-ready;
- it runs the existing Rust NeoReader converter locally and uploads a compact BOOX operation
  manifest instead of the complete PDF;
- it uploads finalized Supernote native page exports;
- it keeps an immutable local cache of accepted Supernote page snapshots so a newer page export
  cannot erase the identity baseline needed to interpret a simultaneous BOOX edit;
- it downloads only objects carrying a valid InkBridge broker producer marker;
- it stages BOOX PDF views as create-only, versioned companion deliveries and installs Supernote operation manifests through a temporary sibling file;
- it persists local hashes, source revisions, pending uploads, delivered generations, and observed
  conflicts;
- it refuses to overwrite a BOOX PDF that contains an unpublished local edit;
- it pauses new uploads whenever the broker's preserved conflict objects are present; conflict
  resolution remains an explicit later workflow rather than an inferred generation comparison.

Canonical state, stroke identity, tombstones, conflict detection, generation preconditions, and
loop prevention remain broker responsibilities. Local modification times and filenames never
decide which device wins.

## Folder contract

Each configured document has its Supernote paths plus either the versioned BOOX companion root or the legacy single-file BOOX path:

    BOOX_Device/Documents/InkBridge/inkbridge-doc-v1-<original-pdf-sha256>/incoming/<versioned>.pdf
    BOOX_Device/Documents/InkBridge/inkbridge-doc-v1-<original-pdf-sha256>/incoming/<versioned>.pdf.inkbridge.json
    BOOX_Device/Documents/InkBridge/inkbridge-doc-v1-<original-pdf-sha256>/outgoing/<finalized>.pdf
    BOOX_Device/Documents/InkBridge/inkbridge-doc-v1-<original-pdf-sha256>/outgoing/<finalized>.pdf.inkbridge.json
    BOOX_Device/Documents/InkBridge/inkbridge-doc-v1-<original-pdf-sha256>/.inkbridge-installed.json
    Supernote_Folder/inkbridge-doc-v1-<original-pdf-sha256>/outgoing/page-0001.json
    Supernote_Folder/inkbridge-doc-v1-<original-pdf-sha256>/incoming/r<revisions>-g<generation>-<event>.operations.json
    Supernote_Folder/inkbridge-doc-v1-<original-pdf-sha256>/acknowledged/<delivery-sha256>.ack.json
    Supernote_Folder/inkbridge-doc-v1-<original-pdf-sha256>/.inkbridge-accepted/r<revision>-<page-id>-<content-sha256>.json

`booxHandoffRoot` points at the local mirror of `/storage/emulated/0/Documents/InkBridge`. When it is configured, `booxPdf` remains only as the legacy/testing fallback required by schema version 1; normal BOOX delivery and upload use the versioned companion directories instead. Each mapped `originalFileName` must then be a safe leaf filename no longer than 180 UTF-8 bytes; configuration validation reports this before the transport starts.

The Supernote adapter or companion writes an export to a temporary part file and renames it to
JSON only after the complete native page export is durable. The folder transport ignores hidden
files and part JSON files. Broker manifests are written only to the separate incoming directory,
so a download cannot be mistaken for a new Supernote export.

`.inkbridge-accepted` is transport-managed cache data, not a device-authored update. Each entry is
content-hash verified and created without overwriting an existing snapshot. The cache is rebuilt
from the broker's immutable accepted upload objects after checkpoint loss, so replacing
`outgoing/page-0001.json` with a newer export does not destroy the earlier baseline.

InkBridge 0.2.0 implements this handoff in the official Supernote plugin. The `.snplg` publishes
native page exports atomically, consumes incoming manifests once, and records durable
acknowledgements without changing the RTL reader's active development tree. See
[`INKBRIDGE_SUPERNOTE_FOLDER_COMPANION.md`](INKBRIDGE_SUPERNOTE_FOLDER_COMPANION.md).

After each successful scan, the transport also atomically publishes
`incoming/inkbridge-status.json`. The plugin uses that checkpoint to distinguish accepted/synced
exports from work that is still pending, and the transport publishes `conflict` or `error` status
without treating either as a device update.

Downloaded Supernote manifests are named
`r<boox-revision>-r<supernote-revision>-g<generation>-<event>.operations.json`, with fixed-width
numeric fields. This preserves causal application order even though event IDs themselves are
unordered.

The shared-folder mirror must include the sibling `acknowledged` directory. The transport will not
upload a Supernote page snapshot while any downloaded operation manifest lacks its matching
content-hash acknowledgement, so a merely delivered BOOX revision cannot be mistaken for an
applied revision. Native exports also carry the revision pair at which they were captured; after a
new manifest advances the transport frontier, an older snapshot is rejected until the user exports
the page again. Manifest downloads require the next causal BOOX revision, so a mirror that exposes
N+1 before N cannot advance local state or the plugin queue.

## Configuration

Copy docs/examples/inkbridge-folder-transport.example.json outside the repository and replace:

- the Cloud Storage bucket;
- each stable document ID, derived from the immutable original PDF;
- the exact logical Supernote filename;
- the Supernote paths, the legacy BOOX fallback path, and `booxHandoffRoot` for the mirrored companion root;
- gcloudCommand when the executable is not on PATH.

Relative paths are resolved next to the configuration file. The state file is local/private and
must not be synced between two running transports. A process lock prevents two transports from
using it concurrently. If the operating system terminates the process without allowing cleanup,
remove the adjacent lock file only after verifying that no transport is still running.

Production scans must use the Terraform-managed `inkbridge-folder-transport` service account,
not project-owner or broker-runtime credentials. That identity can read the private bucket, but its
create-only writes are IAM-conditioned to `BOOX_Folder/` and `Supernote_Folder/`. It cannot
create `Conflicts/.../resolution.json`, canonical state, or broker outbox objects. Resolution
markers are trusted only because this broker-only namespace boundary is enforced by IAM; their
custom metadata is validation data, not authentication.

Set `folder_transport_operator` to the operator's `user:` or `group:` IAM member before the
reviewed Terraform apply. Then use a dedicated gcloud configuration:

```text
gcloud config configurations create inkbridge-folder-transport
gcloud config set project PROJECT_ID --configuration=inkbridge-folder-transport
gcloud config set auth/impersonate_service_account \
  inkbridge-folder-transport@PROJECT_ID.iam.gserviceaccount.com \
  --configuration=inkbridge-folder-transport
```

Select that configuration for the transport process (for example with
`CLOUDSDK_ACTIVE_CONFIG_NAME=inkbridge-folder-transport`). Uploads remain create-only; retrying an
uncertain upload succeeds only when the existing immutable object carries the exact intended hash
and revision metadata.

## Running

Run one scan:

    cargo run -p inkbridge-folder-transport -- once --config C:\InkBridge\transport.json

Keep watching:

    cargo run -p inkbridge-folder-transport -- watch --config C:\InkBridge\transport.json

Inspect the durable local checkpoint without contacting the cloud:

    cargo run -p inkbridge-folder-transport -- status --config C:\InkBridge\transport.json

For a new document, place current Supernote native page exports in its outgoing directory first.
They are uploaded and acknowledged one revision at a time. Only acknowledged exports become the
identity baseline for compact BOOX conversion. A concurrent unacknowledged Supernote change is
uploaded as its own revision while BOOX conversion continues against the immutable accepted cache;
the broker can therefore preserve both inputs and report the intended simultaneous-edit conflict.

## BOOX NeoReader handoff constraint

Real Note Air 4C testing confirmed that NeoReader associates imported annotation state with the
file path. Replacing the bytes of a PDF that is already known at the same path can display the new
ink without making it lassoable. Opening the identical broker view under a fresh path triggers
NeoReader's document-data import/merge flow and makes the strokes editable.

The BOOX companion and folder transport implement that contract. Broker outputs are published as an immutable, versioned PDF plus descriptor, with the descriptor written last. The companion installs the delivery at a fresh active path, records a compact local stroke baseline, publishes a durable acknowledgement, retires the predecessor, and opens only the authoritative active file in NeoReader. On return it runs the shared Rust converter on-device and publishes a compact operation manifest plus `StorageEvent` sidecar. The transport uploads that prebuilt manifest directly, including stale or concurrent manifests with their original `basedOn` frontier so the broker preserves them as conflict evidence instead of silently rebasing them. Full finalized PDFs remain a manual fallback for native conversion/recovery failures. Once broker acceptance is reflected in an installed companion view, the accepted outgoing pair is durably retired through an interruption-recoverable marker.

## Large PDFs

The BOOX companion hashes and parses the active PDF on the tablet after NeoReader returns, but only
the converter's operation JSON enters the mirrored outgoing folder. A 300-500 MB PDF therefore does
not cross the device-to-computer or cloud boundary for every BOOX ink edit. Supernote-to-BOOX changes still download a generated device view because NeoReader needs
the complete editable PDF. The broker continues rebuilding that view from the immutable original.

## Failure behavior

- Repeated scans do not duplicate uploads or downloaded manifests.
- The current acknowledged BOOX delivery pair is retained as active recovery data and is reconstructed from its immutable cloud generation if it disappears. Older dominated incoming pairs are removed and are not recreated even after transport checkpoint loss. Republishing the same installed revision and content at a new storage generation is ignored before download and any equivalent staged pair is retired, while same-revision different-content objects remain an explicit error. The descriptor is always published only after the PDF is durable.
- Accepted finalized BOOX snapshots remain local until the companion installs a broker view containing their BOOX revision. They are then retired through a synchronized marker; a crash during either file deletion completes the same cleanup on the next scan.
- A downloaded Supernote manifest that disappears before its valid acknowledgement is restored
  from the immutable cloud generation; an acknowledged delivery does not need to remain local.
- A process crash after upload retries the same content/revision-stable object name.
- A failed downloaded-content hash never replaces the local destination.
- A pending BOOX edit prevents a broker PDF from overwriting it.
- A stale same-page Supernote export and an out-of-order Supernote manifest are preserved but
  deferred; no implicit latest-file-wins decision is made. A sibling page exported at the same
  original frontier can safely rebase across accepted Supernote-only page revisions, while any
  intervening BOOX revision or accepted revision of that same page requires a fresh export.
- Simultaneous device edits are preserved by the broker under `Conflicts/`; the transport reports
  one conflict per event and stops new source uploads instead of choosing a winner. Resolution
  retains both evidence objects and adds a broker-authenticated `resolution.json` marker. The
  transport ignores resolved groups only when that marker has the expected producer, document,
  event, and kind metadata; forged or incomplete markers remain blocking.
- Accepted Supernote baselines survive working-file replacement and checkpoint loss through the
  immutable `.inkbridge-accepted` cache; a missing or hash-mismatched cache entry is recovered from
  its accepted cloud generation before BOOX conversion proceeds.
- State publication keeps the prior checkpoint until the replacement is complete and recovers from
  a staged prior checkpoint after interruption.
- Windows crash recovery safely retires a staged BOOX predecessor while holding a handle that blocks
  destination replacement. Unix cannot atomically bind deletion of that separate backup pathname to
  the validated destination inode, so it preserves `.previous.part` and stops for explicit
  reconciliation instead of risking data loss.

## Validation

    cargo fmt --all --check
    cargo clippy -p inkbridge-folder-transport --all-targets -- -D warnings
    cargo test -p inkbridge-folder-transport

The tests cover duplicate scans, revision acknowledgement, broker-manifest delivery, bounded versioned BOOX handoff retention and checkpoint recovery, accepted outgoing retirement, immutable
baseline recovery, simultaneous local edits, compact BOOX uploads, unpublished-edit protection,
conflict grouping, forged-marker rejection, and resolution unblocking. Broker and converter suites
remain the authority for stroke parity,
malformed NeoReader recovery, moves, and deletions.
