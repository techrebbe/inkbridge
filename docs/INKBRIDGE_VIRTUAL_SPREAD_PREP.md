# Virtual Spread integration preparation

Status: InkBridge-side prerequisites implemented; representation-specific parsing waits for the
authenticated RTL Reader v0.0.25 contract and `page-143` golden fixture.

## Boundary

The immutable ordinary PDF and annotations expressed in its original-page coordinates remain
canonical. BOOX consumes an editable PDF view. Supernote consumes a private, versioned Virtual
Spread PDF plus native `.mark` state. Neither the generated spread nor its `.mark` is synchronized
as cross-device annotation authority.

The RTL Reader representation producer owns authenticated source-to-spread mappings and view
identity. InkBridge owns canonical annotation state, merge semantics, inverse/forward coordinate
translation, and complete cache hydration. The Nomad companion owns verification, dirty-cache
checkpointing, installation, and atomic activation of a generated view.

## Implemented before the manifest is frozen

### Generic affine validation

`inkbridge-convert::AffineTransform` implements conventional six-coefficient affine arithmetic,
inverse derivation, finite-result checks, relative singularity rejection, and point-set round-trip
validation. It intentionally accepts already authenticated coefficients rather than parsing the
current Virtual Spread sidecar. This keeps the mathematical core reusable without blessing an
interim manifest schema.

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

## Deliberately deferred until the RTL contract lands

- parsing and recomputing the Virtual Spread mapping-authority digest;
- binding concrete CropBox, rotation, destination-rectangle, and coordinate-basis fields;
- validating the deterministic view ID and versioned cache basename;
- selecting the hardware-proven hidden cache directory;
- transforming native Supernote element samples into original-page samples;
- importing complete canonical state into a replacement `.mark`; and
- automatic dirty-cache checkpoint, regeneration, activation, and rollback.

Implementing these against the current manifest would create a second temporary authority and risk
coordinate drift. They begin when RTL Reader publishes the new manifest schema and its exact
`page-143` source PDF, generated PDF, sidecar, expected mapping digest, expected view ID, and
coordinate round-trip vectors.

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
