# Virtual Spread integration preparation

Status: RTL Reader v0.0.25's frozen schema-v3 mapping contract and synthetic page-143 golden
vectors are imported from merged commit `025d870bd73f1133664aa37b8443feb7ce10d12d`.
Production activation remains gated on RTL Reader's separately reviewed real-PDF fixture bundle.

## Boundary

The immutable ordinary PDF and annotations expressed in its original-page coordinates remain
canonical. BOOX consumes an editable PDF view. Supernote consumes a private, versioned Virtual
Spread PDF plus native `.mark` state. Neither the generated spread nor its `.mark` is synchronized
as cross-device annotation authority.

The RTL Reader representation producer owns authenticated source-to-spread mappings and view
identity. InkBridge owns canonical annotation state, merge semantics, inverse/forward coordinate
translation, and complete cache hydration. The Nomad companion owns verification, dirty-cache
checkpointing, installation, and atomic activation of a generated view.

## Implemented contract foundation

### Generic affine validation

`inkbridge-convert::AffineTransform` implements conventional six-coefficient affine arithmetic,
inverse derivation, finite-result checks, relative singularity rejection, and point-set round-trip
validation. The schema-v3 adapter accepts only the authoritative forward matrix and derives its
inverse locally; the inverse and diagnostic host paths never participate in mapping authority.

### Atomic original-page snapshots

The Supernote export parser remains backward compatible with the legacy single-page shape and now
also accepts this schema-independent adapter payload:

```json
{
  "schemaVersion": 1,
  "sourceFileName": "Book.pdf",
  "documentId": "inkbridge-doc-v1-<original PDF SHA-256>",
  "basedOn": { "boox": 4, "supernote": 7 },
  "pages": [
    { "pageIndex": 142, "strokes": [] },
    { "pageIndex": 143, "strokes": [] }
  ]
}
```

Each `pages` entry is a complete snapshot of one **original** PDF page after the Supernote adapter
has applied the inverse Virtual Spread transform. One storage event carries the entire batch under
one Supernote revision. This is essential because both source pages share one physical native
spread and one `.mark` owner.

The parser and broker reject:

- an undeclared multi-page schema;
- duplicate page indices;
- duplicate stable stroke identities across represented halves;
- non-finite or out-of-range normalized samples;
- document IDs or revision frontiers that disagree with the storage event; and
- page indices outside the immutable original.

Applying the batch is atomic. Missing strokes become tombstones only on pages explicitly included
in the batch. A stable ID that moves between the two represented pages remains active at its new
page rather than being deleted and resurrected. Pages outside the batch are untouched. Duplicate
event delivery remains idempotent through the existing processed-event invariant.

Conservative conflict inspection and resolution consume the same batch as one revision. Safe
changes on one half can be merged while an overlapping edit on the other half is preserved for an
explicit decision.

The folder transport keeps a multi-page snapshot as one immutable upload with one source revision.
It also records revision acceptance against every represented original page, so an older export of
either half cannot overwrite a newer spread snapshot. During migration, if a newer legacy one-page
export supersedes only one half of an accepted batch, the local converter materializes only the
still-current half; overlapping baselines are never passed to the BOOX diff engine.

### Schema-v3 representation adapter scaffold

`inkbridge-convert` now has a strict schema-v3 parser which is always bound to the immutable
original PDF SHA-256 and its `inkbridge-doc-v1-*` identity. It rejects duplicate JSON keys, unknown
fields, wrong JSON types, non-finite numbers, non-int32 page indices, incomplete or duplicate page
mappings, invalid rectangles, wrong RTL cover placement, non-uniform/skewed/reflected transforms,
mapping-authority mismatches, and internally inconsistent view/cache identities.

Only the authenticated forward matrix is retained. InkBridge derives the inverse locally and
validates source-normalized-to-spread round trips at the contract tolerance. Canonical points use
displayed-CropBox `[0,1]` coordinates with a top-left origin. Production cache activation is
explicitly disabled until the separate real-PDF/PDF-tail fixture gate lands.

The exact merged `page-143-contract-v1.json` fixture now pins canonical mapping and view bytes,
lowercase SHA-256 identities, zero-based int32 indices, binary64 field order, signed-zero
preservation, positive-orientation quarter-turn transforms, and point/stroke forward/inverse
round trips at an absolute `1e-12`. Contract drift fails closed.

The annotation identity helper preserves a retained `sourceUuid`. If Supernote `userData` loses it,
the adapter derives a document-bound ID from a nonempty native element key. It fails closed rather
than pretending a geometry fingerprint is stable across lasso movement. The native-key path still
requires a real-device reopen/move round trip before production use.

### Versioned cache-regeneration transaction model

`inkbridge-folder-transport` now models regeneration as a durable state machine. A dirty active
view must be exported first. A candidate must have a new document/view-derived cache name, hydrate
every represented source page from one canonical revision, match its generated PDF, sidecar,
mapping authority, and new `.mark` evidence, and retain rollback evidence before activation can be
committed. No transaction field or transition copies an old `.mark` onto a different PDF.

The cache transaction tests use the frozen synthetic view identity to exercise dirty checkpoint,
two-page hydration from one canonical revision, verification, activation evidence, persistence,
and rollback. They do not represent a real generated PDF or native `.mark`.

## Deliberately deferred until the real-PDF fixture bundle merges

- importing the original source PDF, generated Virtual Spread PDF and sidecar, stable artifact
  hashes, exact real-PDF vectors, and descriptor-verified PDF-tail evidence;
- binding the hardware-proven hidden cache directory into Nomad publication code;
- enabling production transform acceptance and cache activation;
- transforming real native Supernote element samples into original-page snapshots;
- importing complete canonical state into a replacement `.mark`; and
- automatic dirty-cache checkpoint, regeneration, activation, and rollback.

The parser and transaction APIs are intentionally representation plumbing rather than activation
authority. The production switch begins only after RTL Reader's fixture-only pull request freezes
and merges the real PDF pair, sidecar, hashes, PDF-tail evidence, and exact real-PDF vectors.

## First integrated acceptance gate

1. Generate and verify the `page-143` Virtual Spread cache.
2. Write a stable-ID stroke on original page 143 through Supernote's native reader.
3. Export the two represented original-page snapshots atomically.
4. Confirm the stroke appears editable at the same location on BOOX page 143.
5. Move it on BOOX and verify the same ID moves on Supernote.
6. Delete it on Supernote and verify a tombstone removes it on BOOX without resurrection.
7. Repeat duplicate delivery and reopen/restart cases.
8. Regenerate the Virtual Spread cache and fully hydrate canonical state before activation.

Pen strokes are the first vertical slice. Native text highlights and source PDFs that already
contain non-link annotations remain separate follow-up work.
