# InkBridge Cloud runtime milestone

This milestone connects the storage-independent broker to production-shaped
Google Cloud interfaces without creating or deploying cloud resources.

## Request flow

Eventarc sends a binary CloudEvent for
`google.cloud.storage.object.v1.finalized` to the private Cloud Run service.
The runtime accepts only objects in `BOOX_Folder/` or `Supernote_Folder/` and
derives device side from that folder. Stable document identity comes from the
`inkbridge-document-id` object metadata, never from a filename.

Device uploads include:

```text
inkbridge-document-id
inkbridge-source-revision
inkbridge-based-on-boox
inkbridge-based-on-supernote
inkbridge-content-sha256        # optional; runtime verifies or computes it
```

Broker outputs already carry the producer, source event, document ID, source
revision pair, and content hash. The Eventarc adapter reconstructs the trusted
broker-output marker so the core records but does not reprocess the output.

Missing/invalid device metadata and a declared SHA-256 that does not match the
finalized generation are permanent input failures. The HTTP adapter logs an
explicit `rejected` result and acknowledges them with HTTP 200 so Eventarc does
not retry an immutable bad event forever. Cloud Storage, Firestore, and pending
outbox failures still return 500 and remain eligible for Eventarc retry.

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
object exists with the expected bytes and metadata does another atomic
Firestore commit promote the pending canonical-state pointer to active and mark
the outbox delivered.

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

An authenticated operator or future folder adapter stages a source PDF outside
the two device folders and calls:

```http
POST /v1/documents/register
Content-Type: application/json

{
  "originalObjectPath": "Staging/my-document.pdf",
  "originalFileName": "my-document.pdf",
  "sourceGeneration": 123
}
```

The broker validates the PDF, derives its stable content-based document ID, and
stores the immutable original under `Originals/<documentId>/original.pdf`.
Registration is idempotent for identical source bytes.

## Cloud Storage layout

```text
Staging/                                  # registration input only
Originals/<documentId>/original.pdf       # immutable source
BOOX_Folder/<documentId>/<name>.pdf       # generated/editable BOOX view
Supernote_Folder/<documentId>/incoming/   # native-operation manifests
Canonical/<documentId>/accepted/          # immutable accepted input evidence
Canonical/<documentId>/states/            # immutable canonical-state blobs
BrokerOutbox/<documentId>/<commitId>/      # immutable staged output payloads
Conflicts/<documentId>/<event>/           # both sides preserved
```

Canonical active state and the durable outbox live in Firestore. Device folder
objects remain generated views; the original is never rewritten.

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

The runtime container includes `inkbridge-convert` and `qpdf` and targets Linux
`amd64`. `infra/gcp` is an opt-in deployment configuration. CI validates three
credential-free, no-apply states: disabled, bootstrap, and runtime. Bootstrap
creates the data plane and regional image repository while omitting Cloud Run
and Eventarc. Runtime requires an immutable Artifact Registry `@sha256` image
and adds those two resources. Real applies use the partial GCS backend so
canonical infrastructure state is not left on one workstation.

`cloudbuild.runtime.yaml` builds the existing runtime Dockerfile as Linux
amd64. The reviewed operator flow tags a source commit, resolves that tag to a
digest, and supplies only the immutable digest to Terraform.
