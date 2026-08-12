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
- it downloads only objects carrying a valid InkBridge broker producer marker;
- it installs BOOX PDF views and Supernote operation manifests through a temporary sibling file;
- it persists local hashes, source revisions, pending uploads, delivered generations, and observed
  conflicts;
- it refuses to overwrite a BOOX PDF that contains an unpublished local edit;
- it pauses new uploads whenever the broker's preserved conflict objects are present; conflict
  resolution remains an explicit later workflow rather than an inferred generation comparison.

Canonical state, stroke identity, tombstones, conflict detection, generation preconditions, and
loop prevention remain broker responsibilities. Local modification times and filenames never
decide which device wins.

## Folder contract

Each configured document has three local paths:

    BOOX_Folder/Example.pdf
    Supernote_Folder/Example/outgoing/page-0001.json
    Supernote_Folder/Example/incoming/<event>.operations.json

The Supernote adapter or companion writes an export to a temporary part file and renames it to
JSON only after the complete native page export is durable. The folder transport ignores hidden
files and part JSON files. Broker manifests are written only to the separate incoming directory,
so a download cannot be mistaken for a new Supernote export.

The current official proof plugin still emits native exports through logcat and embeds an incoming
manifest at package time. This folder contract is the stable handoff for the next on-device
companion integration: it removes manual cloud operations now without coupling cloud state to the
experimental plugin UI or the RTL reader's active development tree.

## Configuration

Copy docs/examples/inkbridge-folder-transport.example.json outside the repository and replace:

- the Cloud Storage bucket;
- each stable document ID, derived from the immutable original PDF;
- the exact logical Supernote filename;
- the three local folder paths;
- gcloudCommand when the executable is not on PATH.

Relative paths are resolved next to the configuration file. The state file is local/private and
must not be synced between two running transports. A process lock prevents two transports from
using it concurrently. If the operating system terminates the process without allowing cleanup,
remove the adjacent lock file only after verifying that no transport is still running.

The user running the transport must already be authenticated with gcloud and authorized for the
private InkBridge bucket. The transport uses create-only uploads with custom metadata; retrying an
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
identity baseline for compact BOOX conversion; a concurrent unacknowledged Supernote change makes
the BOOX upload wait rather than diffing against the wrong state.

## Large PDFs

The BOOX source PDF is hashed and parsed locally after its quiet period, but only the converter's
operation JSON is uploaded. A 300-500 MB PDF therefore does not cross the network for every BOOX
ink edit. Supernote-to-BOOX changes still download a generated device view because NeoReader needs
the complete editable PDF. The broker continues rebuilding that view from the immutable original.

## Failure behavior

- Repeated scans do not duplicate uploads or downloaded manifests.
- A process crash after upload retries the same content/revision-stable object name.
- A failed downloaded-content hash never replaces the local destination.
- A pending BOOX edit prevents a broker PDF from overwriting it.
- Simultaneous device edits are preserved by the broker under Conflicts; the transport reports
  the conflict and stops new source uploads instead of choosing a winner. It resumes only after an
  explicit reconciliation workflow has safely retired those conflict objects.
- State publication keeps the prior checkpoint until the replacement is complete and recovers from
  a staged prior checkpoint after interruption.

## Validation

    cargo fmt --all --check
    cargo clippy -p inkbridge-folder-transport --all-targets -- -D warnings
    cargo test -p inkbridge-folder-transport

The tests cover duplicate scans, revision acknowledgement, broker-manifest delivery, compact BOOX
uploads, unpublished-edit protection, and conflict blocking. Broker and converter suites remain
the authority for stroke parity, malformed NeoReader recovery, moves, and deletions.
