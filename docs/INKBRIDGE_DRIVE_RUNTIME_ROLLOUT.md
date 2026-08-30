# Google Drive runtime rollout (not deployed)

This document describes the reviewed runtime package and the later deployment
shape. Merging the code does **not** create a Cloud Run Job, Scheduler trigger,
OAuth refresh token, or billable polling workload.

## Runtime boundary

`inkbridge-drive-runtime` is a short-lived job. It reads one durable Drive
change page, performs all required downstream durability steps, and advances
the Drive page token only after the complete page is safe.

```text
Drive changes.list
  -> exact file-version download and size/hash verification
  -> immutable create-only Cloud Storage evidence
  -> Firestore pending checkpoint (compare-and-swap)
  -> existing private broker processing
  -> accept/reject checkpoint
  -> reserve broker delivery
  -> query exact inkbridgeDeliveryId
  -> Drive files.create or reconcile existing file
  -> bind returned Drive file ID/version
  -> advance Drive page token
```

The job defaults to dry-run. Production mutation requires the explicit
`--apply` argument. Dry-run downloads and validates a change page but never
uploads evidence, calls the broker, creates a Drive file, or advances the page
token.

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

Every update uses the exact Firestore `updateTime` as a precondition. A second
poller, a stale retry, or an operator edit therefore fails closed instead of
overwriting a newer page token or output reservation.

Pending inputs retain the exact immutable GCS path and generation. Pending
outputs retain the exact broker-output GCS path and generation. Those fields
allow a new container instance to resume after process termination without
guessing which bytes it should deliver.

## Proposed deployment (later approval)

1. Build `Dockerfile.drive-runtime` for Linux `amd64` and pin its Artifact
   Registry digest.
2. Create a Cloud Run **Job**, not a continuously running service. Configure
   one task, no parallelism, `--apply`, and the environment above.
3. Grant its service account only:
   - read/write on the private InkBridge bucket;
   - read/write on the two InkBridge Firestore collections;
   - accessor on the two named OAuth secrets;
   - no broad project-owner role.
4. Run dry-run manually against disposable Drive files.
5. Run one manual `--apply` execution and verify Drive, GCS, Firestore, and
   broker state before enabling a schedule.
6. Add Cloud Scheduler only after the repeated E2E gate. A conservative
   interval is sufficient because native device sync is not real-time.

No Terraform resource for the job or scheduler is active in this PR. That
keeps deployment, IAM changes, OAuth publication, and recurring cost behind a
separate explicit approval.

## Recovery guarantees

- If GCS upload succeeds before a crash, retry verifies and reuses the exact
  immutable generation.
- If the broker commits before a crash, retry processes the pending event
  idempotently and recovers its generated view.
- If Drive creates an output before checkpointing its file ID, retry queries
  the exact private `inkbridgeDeliveryId` and binds the existing file.
- If more than one Drive file claims the delivery ID, the job stops for repair.
- If Firestore `updateTime` changes, the stale job stops without advancing the
  Drive token.
- If the broker permanently rejects an event, the pending input is cleared but
  its file frontier/hash is not advanced; a corrected new Drive revision keeps
  the true causal base.
