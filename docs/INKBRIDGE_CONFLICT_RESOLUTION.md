# InkBridge conflict resolution

InkBridge stops when both devices advance from the same last-common revision. It preserves both
inputs and asks for an explicit decision; it never chooses the newest timestamp or filename.

This first workflow is deliberately conservative and API-driven. A later companion UI can present
the same analysis and choices without exposing revision terminology.

## Inspection

List the document's active conflicts first:

```text
GET /v1/documents/<document-id>/conflicts
```

Each summary includes the raw conflict event ID needed for detailed inspection and resolution.

Inspect one active conflict through the private Cloud Run service:

```text
GET /v1/documents/<document-id>/conflicts/<conflict-event-id>
```

The response contains:

- the current canonical BOOX/Supernote revision pair and state revision;
- the incoming side, source revision, and `basedOn` pair;
- `safeChanges`: additions or changes whose canonical stroke did not change after `basedOn`;
- `overlappingChanges`: strokes changed by both branches, including deletion-versus-edit cases.

Each change reports the stable stroke ID, page, and kind (`add`, `move`, `update`, or `delete`). The
analysis is a snapshot. Resolution must echo its state revision and current revision pair so a
later update cannot be overwritten using stale analysis.

## Choices

`merge_preserving_current` is the recommended single-user default. It applies only `safeChanges`
and keeps the current canonical version for every overlap. The broker then sends corrective device
views so both devices converge on that explicit result.

`keep_current` rejects every incoming annotation change, but still consumes the incoming device
revision and sends the current canonical view back to that device. Advancing the rejected source
frontier is required to prevent the same conflict from recurring.

`accept_incoming` applies every incoming change, including overlaps and deletions. Use it only when
the incoming device version is intentionally authoritative.

Resolve through:

```text
POST /v1/documents/<document-id>/conflicts/<conflict-event-id>
Content-Type: application/json

{
  "resolutionId": "user-resolution-uuid",
  "expectedStateRevision": 7,
  "expectedCurrentRevisions": { "boox": 2, "supernote": 5 },
  "strategy": "merge_preserving_current"
}
```

`schemaVersion` defaults to the current resolution schema. Repeating the same `resolutionId` with
the same recorded strategy is idempotent. Reusing that ID with a different strategy, submitting a
different resolution for an already-resolved conflict, using stale analysis, or encountering a
changed destination generation returns HTTP 409 and commits nothing.

## Persistence and transport behavior

The broker atomically commits:

1. a BOOX PDF rebuilt from the immutable original and resulting canonical state;
2. a Supernote operation manifest when that device needs changes or corrections;
3. updated canonical state and a `resolvedConflicts` audit record;
4. `Conflicts/<document>/<event>/resolution.json` with broker-generated metadata.

The original incoming and competing evidence objects are never deleted by resolution. The folder
transport groups all evidence under one event into one conflict status. It resumes only when the
same group contains a valid broker-generated resolution marker; forged or incomplete markers do
not unblock it. In the cloud runtime that marker is published only after the corresponding
canonical state is active, and its durable pending phase is recoverable.

Full NeoReader PDFs and compact BOOX operation manifests are both supported. Compact inputs receive
the same identity, geometry, style, page-bound, and fingerprint validation used by normal broker
processing. Only operations that still match the based-on canonical stroke qualify for automatic
safe merging.
Page-scoped Supernote exports and compact BOOX manifests are incremental, so active conflicts from
either source must be resolved in source-revision order. A newer incremental conflict is rejected
until every earlier active source revision is resolved, preventing a later page or operation delta
from silently superseding an earlier one. Full NeoReader PDFs remain self-contained snapshots.

## Current UX boundary

The endpoints are private operational APIs, not the finished conflict UI. Until a companion UI is
added, an operator inspects the response and chooses one of the three strategies. The future UI
should summarize:

- what BOOX changed;
- what Supernote changed;
- which changes can coexist;
- which overlapping strokes will be retained from which side.

It should default to `merge_preserving_current`, require confirmation for `accept_incoming`, and
never offer an unqualified “latest wins” action.

Terraform grants `roles/run.invoker` only to Eventarc and the configured
`folder_transport_operator`; it does not grant `allUsers` or `allAuthenticatedUsers`. The service
accepts network ingress so that operator can reach the IAM-protected API. From an authenticated
operator configuration, start a local proxy:

```text
gcloud run services proxy inkbridge-broker \
  --project=PROJECT_ID \
  --region=REGION \
  --port=8080 \
  --configuration=inkbridge-operator
```

Then issue the GET and POST requests above against `http://localhost:8080`.

## Validation

Synthetic tests cover all strategies, safe/overlapping classification, compact BOOX inputs,
idempotent retries, stale analysis, stale destinations, retained evidence, forged markers, and
Cloud Run recovery/outbox behavior.

The actual Note Air 4C/Nomad simultaneous-edit evidence can be replayed locally:

```bash
INKBRIDGE_CONFLICT_FIXTURE_ROOT=/path/to/inkbridge-runs/e2e-925715a-20260823 \
  cargo test -p inkbridge-broker \
  real_device_simultaneous_edit_evidence_resolves_without_losing_either_side \
  -- --ignored --exact
```

That replay reconstructs the accepted b2/s5 state, ingests the preserved concurrent BOOX PDF,
keeps the Supernote-only handwriting, applies the safe BOOX-only handwriting, and converges at
b3/s5.