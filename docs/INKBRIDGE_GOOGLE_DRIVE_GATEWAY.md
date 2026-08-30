# InkBridge Google Drive gateway

Google Drive is the user-facing file transport for InkBridge. It is not the
canonical synchronization database. Cloud Storage retains immutable revision
evidence, Firestore retains the active canonical pointer and durable outbox,
and the broker remains the only component allowed to merge annotation state.

## User-visible layout

```text
InkBridge/
  BOOX_Folder/         ordinary PDFs plus versioned InkBridge deliveries
  Supernote_Folder/    ordinary PDFs plus native export/import artifacts
```

The two folders contain device-specific representations of the same logical
documents. They must remain separate. A filename is presentation only: the
gateway binds a Google Drive file ID to an `inkbridge-doc-v1-<sha256>` identity
derived from the clean immutable original PDF.

The Virtual Spread PDF, authenticated mapping sidecar, and Supernote `.mark`
remain hidden device-local cache artifacts. They are not uploaded to Drive.

## Onboarding a document

1. Put the same clean original PDF in both device folders before annotating it.
2. The onboarding flow asks the user to confirm that each exact Drive file
   version is the clean original. Its approval records file ID, Drive version,
   and SHA-256; the background watcher cannot invent this approval.
3. The gateway verifies byte length and that exact approval, rejects every
   InkBridge-generated PDF, and submits a create-only registration object to
   Cloud Storage.
4. Equal original bytes converge on one stable document ID even when the files
   have different names.
5. Only after registration succeeds does the gateway persist each Drive file
   ID as the BOOX or Supernote representation of that document.

The runtime captures its first Drive change cursor before enumerating both
device folders, then processes that initial snapshot before saving the cursor.
An exact approval is read from `inkbridgeDriveApprovals/<drive-file-id>` and is
verified by the existing registration/association planner. An unapproved
ordinary file blocks cursor advancement; it is never silently consumed.

An already-annotated unbound PDF is not safe to auto-register because its byte
hash no longer identifies the clean original. It requires explicit recovery or
manual binding.

## Drive to broker

The scheduled gateway reads the Drive change feed from its durable page token.
The first companion-created BOOX manifest or Supernote native export has a new
Drive file ID, so it uses an authenticated association action that names the
existing document and device side and approves the exact file ID, Drive
version, SHA-256, and causal base revisions. The gateway checks the expected
folder and payload type and rejects either causal component if it is ahead of
the broker's current frontier. It then persists the binding and frontier and
processes the same revision normally. It never associates a device artifact by
filename.

For every bound file revision it:

- rejects incomplete downloads by comparing received bytes with Drive's size;
- refetches the full Drive file metadata after each download and rejects the
  bytes if version, size, parent, MIME type, deletion state or private
  properties changed during transfer;
- requires a bound file to remain directly in the configured folder for its
  device side;
- suppresses metadata-only Drive versions when that file's downloaded SHA-256
  matches its last accepted content;
- derives an event ID from file ID, Drive version, and SHA-256 for real content
  revisions;
- reads the broker's current revision frontier while retaining the bound file's
  durable observed frontier as the event's causal `basedOn` value;
- allocates the source revision immediately after the newer of those two source
  frontiers. An edit to an older delivery therefore remains visibly stale but
  cannot be assigned a revision the broker would silently discard. The broker
  preserves distinct late same-side evidence as a conflict; after each accepted
  edit, that file frontier advances on its source side so repeated edits uploaded
  before Eventarc catches up still chain sequentially;
- creates an immutable object under `BOOX_Folder/` or `Supernote_Folder/` with
  the same metadata contract used by Eventarc;
- records the event as pending only after the Cloud Storage create succeeds;
- advances that file's observed frontier and accepted hash only after the
  broker accepts the object; broker rejection clears the pending record without
  advancing either value, so a corrected revision retains the real causal base;
- checks for a pending file before duplicate/hash suppression, defers every later
  version while one input is pending, and retains the Drive page token until the
  broker outcome is durable—even if the later bytes revert to an older hash;
- advances the Drive page token only after the complete page is durable.

Duplicate change delivery therefore produces the same event and cannot create
duplicate annotations. A rename or other metadata-only update is harmless even
though Drive increments its version: the per-file accepted content hash keeps
it from consuming a canonical source revision. File ID, original bytes, and
canonical document ID—not path or modification time—carry identity.

BOOX supplies either a NeoReader PDF revision or, for normal large-document
operation, the companion's compact operation manifest. The integrated BOOX
Drive uploader is a convenience for small PDFs only: hardware testing showed a
210 MiB document embedding locally without uploading the changed PDF, matching
the documented 200 MB reading-data limit. The 300–500 MB path must keep using
local conversion to a small operation manifest.

Supernote supplies native page exports. A rooted companion will eventually
wrap the native Sync action with export-before-sync and import-after-sync so
the normal Supernote Sync button remains the only routine user action.

## Broker to Drive

Broker outputs are copied to a target device folder as new versioned files.
The gateway never overwrites a Drive destination selected by "latest file".
Each file carries private `appProperties` with the broker producer, source
event, document ID, revision pair, content hash, Cloud Storage generation, and
delivery ID. A repeated job recognizes the delivery ID and does nothing.

Before calling `files.create`, the gateway durably reserves the delivery ID and
queries Drive for that exact private property. Only a confirmed absence permits
the create. If the process crashes after Drive creates the file but before the
checkpoint records the returned identity, the retry reconciles and binds the
existing file rather than creating a visible duplicate. Multiple matches fail
closed for operator repair.

After Drive confirms or reconciliation recovers the create, the gateway records
the returned file ID and version, binds that file to its document and target
device, clears the reservation, and marks that exact generated revision as
processed. This prevents the initial broker output from re-entering the broker.
A later user edit of the same file has a new Drive version and content hash, so
it enters the normal inbound path even though Drive preserves the private
InkBridge properties. An unbound file carrying an InkBridge producer property
is ignored rather than guessed or registered.

## Authentication and access boundary

The personal My Drive files are accessed through offline OAuth for the owner's
Google account. A service account is not used because service accounts do not
have personal Drive storage quota and cannot own ordinary My Drive files.

The Drive API does not provide a durable folder-scoped OAuth permission that
can reliably observe files created by BOOX and Supernote. The OAuth grant is
therefore the full Drive scope, while the gateway enforces the two configured
folder IDs and persisted file bindings in code. The OAuth client and refresh
token belong in Secret Manager, never in Drive, Firestore, logs, repository
files, container images, or Terraform state.

During development the OAuth app remains in Testing and only the owner's email
is a test user. Testing-mode refresh tokens for Drive access expire after seven
days. Production linking must wait for the reviewed gateway deployment, a
public privacy statement, and deliberate publication of the OAuth consent
screen.

## Deployment sequence

The storage-independent planning crate is now wrapped by the separately
packaged `inkbridge-drive-runtime` Cloud Run Job. The job remains undeployed and
defaults to dry-run. See `INKBRIDGE_DRIVE_RUNTIME_ROLLOUT.md` for the exact
durability sequence, configuration, and later approval gates. The reviewed
rollout remains intentionally split:

1. merge deterministic registration, inbound, outbound, idempotency, and loop
   prevention rules;
2. add Drive/Cloud Storage/Firestore/Secret Manager adapters and a default-safe
   Cloud Run Job package that scales to zero between polls (implemented, not
   deployed);
3. add Scheduler with a conservative interval and a lease so only one poller
   owns a checkpoint;
4. authorize the owner account once, store the refresh token in Secret Manager,
   and run a dry synchronization against disposable files;
5. connect device-native sync flows and perform the full repeated E2E gate.

No Drive change is canonical merely because it is newer. Concurrent BOOX and
Supernote revisions retain their original `basedOn` frontier and are handed to
the broker's existing conservative conflict path.

## Core validation

```text
cargo fmt --all --check
cargo clippy -p inkbridge-drive-gateway --all-targets -- -D warnings
cargo test -p inkbridge-drive-gateway
```

Tests cover stable identity across rename, matching clean originals from both
folders, duplicate Drive events, metadata-only version suppression,
authenticated first-device-artifact association, generated-output loop
suppression, per-file causal-frontier preservation, pending-upload sequencing,
broker rejection rollback, retry-safe original registration, refusal to guess
an unbound file from its name, create-only broker delivery, and explicit
page-token commit. The outbound lifecycle test also proves that the exact
created revision is suppressed while a subsequent device edit of the same
Drive file is accepted under the original stable document identity.
