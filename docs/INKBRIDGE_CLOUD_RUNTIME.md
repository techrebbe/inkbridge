# InkBridge Cloud runtime milestone

This milestone connects the storage-independent broker to production-shaped
Google Cloud interfaces without creating or deploying cloud resources.

## Request flow

Eventarc sends a binary CloudEvent for
`google.cloud.storage.object.v1.finalized` to the private Cloud Run service.
The runtime accepts device updates in `BOOX_Folder/` or `Supernote_Folder/` and
derives device side from that folder. It also accepts explicitly marked
registration objects in `Staging/`; unmarked staging objects are ignored.
Stable document identity comes from original PDF bytes or the
`inkbridge-document-id` object metadata, never from a filename.

Device uploads include:

```text
inkbridge-document-id
inkbridge-source-revision
inkbridge-based-on-boox
inkbridge-based-on-supernote
inkbridge-content-sha256        # optional; runtime verifies or computes it
inkbridge-sync-ready=true       # explicit finalized/closed-file signal
inkbridge-payload-kind          # device_view (default) or boox_operation_manifest
```

Broker outputs already carry the producer, source event, document ID, source
revision pair, and content hash. The Eventarc adapter reconstructs the trusted
broker-output marker so the core records but does not reprocess the output.

Missing/invalid device metadata and a declared SHA-256 that does not match the
finalized generation are permanent input failures. The HTTP adapter logs an
explicit `rejected` result and acknowledges them with HTTP 200 so Eventarc does
not retry an immutable bad event forever. Cloud Storage, Firestore, and pending
outbox failures still return 500 and remain eligible for Eventarc retry.

## Private conflict API

The same private Cloud Run service exposes:

```text
GET  /v1/documents/<document-id>/conflicts
GET  /v1/documents/<document-id>/conflicts/<conflict-event-id>
POST /v1/documents/<document-id>/conflicts/<conflict-event-id>
```

The collection GET returns active conflict summaries and the raw event IDs needed by the item
routes. The item GET returns safe and overlapping stroke changes plus the state/revision
preconditions required by POST. POST accepts an explicit `merge_preserving_current`,
`keep_current`, or `accept_incoming` decision. The runtime recovers any pending durable outbox.
stale destination generations, and competing decisions return HTTP 409; malformed requests return
400; a missing document/conflict returns 404. Cloud Run IAM remains the authorization boundary.

Resolution uses the existing transactional reservation, generation-conditional object writes, and
durable outbox. Every required BOOX/Supernote output is made durable before canonical state is
promoted. The broker-generated `resolution.json` unblock marker is a post-finalization release
write: it cannot become visible until that canonical state is active. See
[`INKBRIDGE_CONFLICT_RESOLUTION.md`](INKBRIDGE_CONFLICT_RESOLUTION.md).

## Firestore transaction and durable outbox

`inkbridgeDocuments/{documentId}` stores a generation/hash pointer to the last
fully published immutable canonical-state blob plus, while work is in flight, a
pending commit reservation.
`inkbridgeOutbox/{commitId}` stores a durable copy of that reservation and each
delivered object generation.

Before reservation, each output payload is staged once under a content-stable
Cloud Storage outbox path. Firestore stores only its immutable generation/hash
pointer, never the PDF or manifest bytes. The runtime then uses an atomic
Firestore `documents:commit` with document `updateTime` preconditions to reserve
both records. It performs destination uploads with `ifGenerationMatch`; after
each output, the outbox records the returned generation. Only after every
ordinary output exists with the expected bytes and metadata does another atomic
Firestore commit promote the pending canonical-state pointer to active. Conflict-resolution
commits retain their pending reservation while the post-finalization marker is delivered, then
atomically clear the pending state and mark the outbox delivered. A crash in either phase resumes
from the recorded object generations without exposing a premature unblock marker. The runtime then
deletes only the finalized output
payload generations from `BrokerOutbox/`; the canonical-state payload remains
the active state evidence. Cleanup is best-effort after finalization, so failure
can leak storage but cannot invalidate published state or block recovery.

This ordering provides these invariants:

- a duplicate Eventarc delivery resumes or reports the same logical commit;
- a crash after an object upload does not duplicate the object on retry;
- active canonical state never points past undelivered device views;
- a stale destination generation cannot be overwritten;
- a competing Firestore reservation fails its compare-and-swap instead of
  silently winning by timestamp.

The Firestore records contain pointers and revision metadata only. They still
fail before a write at a conservative 900 KB; a later scale milestone can shard
very large delivered-object inventories before they approach Firestore's
per-document limit.

## Immutable original registration

The deployed registration path stays private and reuses the existing
authenticated Eventarc delivery. An authorized operator or future folder
adapter uploads the original to `Staging/` with:

```text
inkbridge-register-original=true
inkbridge-original-file-name=<logical file name>
```

The finalized-object event reaches the same internal-only Cloud Run endpoint as
device events. The broker verifies the exact object generation, validates the
PDF, derives its stable content-based document ID, and stores the immutable
original under `Originals/<documentId>/original.pdf`. Duplicate delivery is
idempotent. Unmarked staging objects are ignored. The HTTP
`/v1/documents/register` handler remains useful for local adapter tests, but the
reviewed cloud deployment does not expose or rely on it.

## Cloud Storage layout

```text
Staging/                                  # registration input only
Originals/<documentId>/original.pdf       # immutable source
BOOX_Folder/<documentId>/<name>.pdf       # generated/editable BOOX view
Supernote_Folder/<documentId>/incoming/   # native-operation manifests
Canonical/<documentId>/states/            # immutable canonical-state blobs
BrokerOutbox/<documentId>/<commitId>/      # immutable staged output payloads
Conflicts/<documentId>/<event>/           # both inputs + durable resolution marker
```

Canonical active state and the durable outbox live in Firestore. Device folder
objects remain generated views; the original is never rewritten.

## Large-document behavior

Full-PDF device views remain supported, but the broker reads the exact finalized
Cloud Storage generation once and records that immutable generation as revision
evidence. It no longer creates a second `Canonical/accepted` copy of every
300–500 MB BOOX update. Binary payload handles are reference counted inside one
request, converter hashing is streamed in 1 MiB chunks, and the original input
buffer is released after PDF parsing before serialization begins.

The intended BOOX folder adapter should run `inkbridge-convert` locally and
upload the usually small JSON result with
`inkbridge-payload-kind=boox_operation_manifest`. That path produces the same
Supernote operations as uploading the complete NeoReader PDF, while avoiding a
full document upload and Cloud Run parse for ink-only changes. Until that
adapter ships, a BOOX full-PDF edit still transfers the whole object because
Cloud Storage finalized events contain whole immutable objects, not byte diffs.
Generated objects use Cloud Storage resumable uploads, so the runtime streams a
shared payload buffer instead of constructing a second full multipart body.

The deployment reserves 2 vCPU and 8 GiB only while a request is active, uses
concurrency one, allows up to 15 minutes for unusually large PDFs, and still
scales to zero. Live `Staging/` objects expire after one day and live
`Staging/` archived versions expire one day later. `BrokerOutbox/` is excluded
from age-based lifecycle cleanup because a pending Firestore commit may need an
exact payload generation for arbitrarily delayed recovery. The runtime removes
those payloads by generation only after successful finalization. Originals,
device generations that may still be canonical evidence, and conflicts are
intentionally not auto-deleted.

## Authentication and local validation

On Cloud Run the runtime obtains a short-lived access token from the metadata
server. `INKBRIDGE_GOOGLE_ACCESS_TOKEN` exists only for controlled local tests;
do not persist it. Required runtime variables are:

```text
INKBRIDGE_GCP_PROJECT
INKBRIDGE_GCS_BUCKET
INKBRIDGE_FIRESTORE_DATABASE  # defaults to (default)
PORT                          # defaults to 8080
```

The runtime container includes `inkbridge-convert` and qpdf 12.2 or newer and
targets Linux `amd64`. The minimum qpdf version is enforced while the image is
built: qpdf 11.3 can repair NeoReader's malformed cross-reference table while
silently omitting annotations that qpdf 12.2+ preserves. `infra/gcp` is an
opt-in deployment configuration. CI validates three
credential-free, no-apply states: disabled, bootstrap, and runtime. Bootstrap
creates the data plane and regional image repository while omitting Cloud Run
and Eventarc. Runtime requires an immutable Artifact Registry `@sha256` image
and adds those two resources. Real applies use the partial GCS backend so
canonical infrastructure state is not left on one workstation.

`cloudbuild.runtime.yaml` builds the existing runtime Dockerfile as Linux
amd64. Source archives are submitted through the dedicated private build-source
bucket, so the builder account has no read access to the device-data bucket.
The reviewed operator flow tags a source commit, resolves that tag to a digest,
adds the protected `deployed-current` tag, and supplies only the immutable
digest to Terraform. Cleanup expires old `build-` tags while the protected
deployed tag prevents the Cloud Run digest from being removed.

The opt-in local large-document gate used for this milestone is:

```text
INKBRIDGE_LARGE_DOCUMENT_MIB=300 cargo test -p inkbridge-broker \
  large_document_round_trip_does_not_create_an_accepted_pdf_copy -- --ignored --exact
```
