# Google Drive runtime rollout (not deployed)

This document describes the reviewed runtime package and the later deployment
shape. Merging the code does **not** create a Cloud Run Job, Scheduler trigger,
OAuth refresh token, or billable polling workload.

## Runtime boundary

`inkbridge-drive-runtime` is a short-lived job. It reads one durable Drive
change page, performs all required downstream durability steps, and advances
the Drive page token only after the complete page is safe.

On a fresh checkpoint it first captures a Drive start token and then enumerates
both configured device folders. This initial snapshot is processed before the
token is persisted. Changes racing with the snapshot are therefore covered by
the captured change-feed cursor, while existing files are never skipped. An
unbound ordinary file without an exact onboarding approval blocks cursor
advancement instead of disappearing from the feed.

Approved clean originals are processed before dependent device artifacts in
the same snapshot or change page, regardless of Drive's file ordering. After
the immutable evidence object is staged, the runtime synchronously asks the
broker to parse and register that exact GCS generation. Only a successful,
identity-matching broker registration permits the Drive binding or page cursor
to be committed. A malformed original therefore leaves retry evidence but
cannot create a checkpoint that names a nonexistent canonical document.

```text
Drive changes.list
  -> exact file-version download and post-download metadata recheck
  -> immutable create-only Cloud Storage evidence
  -> Firestore pending checkpoint (compare-and-swap)
  -> existing private broker processing
  -> accept/reject checkpoint
  -> pre-generate a Drive file ID and reserve it with the broker delivery
  -> query exact inkbridgeDeliveryId
  -> Drive files.create with the reserved ID or reconcile that exact file
  -> bind returned Drive file ID/version
  -> advance Drive page token
```

The job defaults to dry-run. Production mutation requires the explicit
`--apply` argument. A first dry-run enumerates the current device folders; later
dry-runs inspect the next change page. Both download and validate relevant
files. When an approved original and dependent artifact share the page, dry-run
first runs the broker's same non-mutating PDF/page validation, then simulates
their binding in memory so it can validate the complete onboarding sequence. A
genuinely new document uses a zero revision frontier; a second device copy of
an existing document reads and preserves its real canonical frontier. Dry-run
never persists that simulation, uploads evidence, invokes a mutating broker
path, creates a Drive file, or advances the page token.

## Required configuration

| Variable | Purpose |
| --- | --- |
| `INKBRIDGE_GCP_PROJECT` | GCP project containing private state |
| `INKBRIDGE_GCS_BUCKET` | immutable evidence/output bucket |
| `INKBRIDGE_FIRESTORE_DATABASE` | defaults to `(default)` |
| `INKBRIDGE_DRIVE_BOOX_FOLDER_ID` | exact BOOX device-folder ID |
| `INKBRIDGE_DRIVE_SUPERNOTE_FOLDER_ID` | exact Supernote device-folder ID |
| `INKBRIDGE_DRIVE_OAUTH_CLIENT_SECRET` | Secret Manager ID containing the downloaded OAuth client JSON |
| `INKBRIDGE_DRIVE_REFRESH_TOKEN_SECRET` | Secret Manager ID containing only the owner's refresh token |
| `INKBRIDGE_DRIVE_CHECKPOINT_ID` | defaults to `primary` |

The two OAuth secrets are read at job startup. Secret bytes are never placed in
Drive metadata, Firestore checkpoint payloads, Cloud Storage metadata, logs,
container layers, or Terraform state.

## Firestore state

The job stores one opaque checkpoint document at:

```text
inkbridgeDriveGateways/<checkpoint-id>
```

Explicit onboarding approvals are read from:

```text
inkbridgeDriveApprovals/<drive-file-id>
```

Each document contains one base64 JSON `approval` payload. The payload names
the exact Drive file ID, version, content SHA-256, device side and—when binding
a device artifact—the existing document ID and causal frontier. The runtime is
read-only for this collection: approval creation remains a deliberate setup/UI
operation and cannot be inferred from a filename.

Every update uses the exact Firestore `updateTime` as a precondition. A second
poller, a stale retry, or an operator edit therefore fails closed instead of
overwriting a newer page token or output reservation.

Pending inputs retain the exact immutable GCS path and generation. Pending
outputs retain the exact broker-output GCS path and generation plus a Drive ID
obtained from `files.generateIds`. Those fields allow a new container instance
to resume after process termination without guessing which bytes it should
deliver. The reserved ID is committed in Firestore before any external Drive
create. Concurrent workers therefore use the same identity; Drive accepts at
most one create, and a retry that receives HTTP 409 reads and verifies that
exact file instead of creating a visible duplicate.

## Default-safe deployment configuration (later approval)

1. Build `Dockerfile.drive-runtime` for Linux `amd64` and pin its Artifact
   Registry digest.
2. The guarded Terraform configuration creates a Cloud Run **Job**, not a
   continuously running service. It configures one task, no parallelism, zero
   retries, and the environment above. Its default argument list is empty, so
   every execution is dry-run.
3. Grant its service account only:
   - object access on the private InkBridge bucket;
   - `datastore.user` in the InkBridge project (Firestore IAM cannot be scoped
     to individual collections; the runtime itself permits only its checkpoint
     collections and the broker's canonical store);
   - accessor on the two named OAuth secrets;
   - no broad project-owner role.
4. Run dry-run manually against disposable Drive files. No Scheduler resource
   exists, so the job cannot poll unless an authenticated operator invokes it.
5. Enable `--apply` only through a newly reviewed plan carrying the separate
   exact acknowledgement. Run one manual apply execution and verify Drive, GCS, Firestore, and
   broker state before enabling a schedule.
6. Add Cloud Scheduler only after the repeated E2E gate. A conservative
   interval is sufficient because native device sync is not real-time.

Terraform now describes the job, empty Secret Manager containers, and
least-privilege IAM, but remains inert behind the existing deployment guard.
It never manages secret versions. No resource is applied by this PR or CI, and
there is still no Scheduler resource. Deployment, OAuth token storage, job
execution, apply mode, and recurring polling therefore remain separate explicit
approval gates.

## Recovery guarantees

- If GCS upload succeeds before a crash, retry verifies and reuses the exact
  immutable generation.
- If the broker commits before a crash, retry processes the pending event
  idempotently and recovers its generated view.
- If Drive creates an output before delivery completion is checkpointed, retry
  queries the exact private `inkbridgeDeliveryId` and verifies the previously
  reserved file ID. The same pre-generated ID makes concurrent or retried
  creates idempotent at Drive as well as in Firestore.
- If more than one Drive file claims the delivery ID, the job stops for repair.
- If Firestore `updateTime` changes, the stale job stops without advancing the
  Drive token.
- If Drive reports a different file revision after a media download, the job
  discards those bytes and retains the page token for a clean retry.
- If an existing or newly observed file has not been explicitly approved for
  original registration or artifact association, the job retains the page
  token and stops rather than silently skipping it.
- If a dependent artifact sorts before its approved original, the runtime
  registers the original first so the broker frontier exists before binding or
  processing the artifact.
- If the broker cannot parse or register an approved original, no Drive-file
  binding and no page cursor is committed. A retry reuses the immutable staged
  evidence generation.
- If the broker permanently rejects an event, the pending input is cleared but
  its file frontier/hash is not advanced; a corrected new Drive revision keeps
  the true causal base.
