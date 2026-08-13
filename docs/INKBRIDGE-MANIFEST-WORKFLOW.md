# InkBridge manifest workflow

InkBridge keeps the manufacturer readers in place:

```text
Supernote DOC/NOTE
  -> Export Page Test (portable baseline)
  -> BOOX NeoReader
  -> Embed Data to PDF
  -> inkbridge-convert (add/update/delete manifest)
  -> Supernote Apply InkBridge Sync
```

This is the first generic implementation of the native-environment bridge. It
replaces the document-specific fixtures used by the hardware proofs.

## 1. Capture the Supernote baseline

For each annotated page that will participate in the round trip, run **Export
Page Test** and save the `INKBRIDGE_EXPORT` log lines. The converter accepts
either that log or the reconstructed JSON payload. Every baseline supplied to
one conversion must name the same source document; mixed-document exports are
rejected before a manifest is created.

## 2. Embed NeoReader data

Open the same PDF in NeoReader, make edits, and use **Embed Data to PDF**. The
PDF is the BOOX-side transport for this milestone.

## 3. Create a manifest

```bash
cargo run -p inkbridge-convert -- extract \
  --pdf NeoReader-embedded.pdf \
  --baseline Supernote-page-1.log \
  --baseline Supernote-page-2.log \
  --output inkbridge-manifest.json
```

NeoReader sometimes writes a malformed incremental cross-reference stream even
though it can reopen the PDF itself. The converter now retries those files
through `qpdf` automatically. Install `qpdf` or set `INKBRIDGE_QPDF` to its
executable when the converter reports that recovery is unavailable.

The result contains portable, page-normalized operations:

- `upsert_stroke` for new or changed handwriting;
- `delete_stroke` for a baseline stroke no longer present in the embedded PDF;
- stable source identities;
- native Supernote style and pressure data where the baseline supplies them;
- a deterministic geometry/style/layer fingerprint for safe repeated application.

Native BOOX `#ONYX-STROKE` geometry comes from NeoReader's vector `/AP`
appearance stream, which the Note Air 4C hardware proof established as the
rendered source of truth. Standard PDF `/Ink` geometry comes from `/InkList`.
Grouped `/InkList` annotations are rejected because PDF provides no stable
identity for each inner path; using the mutable array position would corrupt
identity after insertion or deletion. If any supported annotation on a page
cannot be extracted, deletion inference is disabled for that page so damaged
PDF data cannot erase valid Supernote ink. When the damaged annotation still
exposes a stable identity, that identity also remains active globally, covering
the case where a stroke moved across pages before its destination data became
unreadable.

## 4. Build the Supernote sync package

```bash
supernote-poc/build.sh inkbridge-manifest.json
```

Install the resulting `.snplg`, open the target document, and tap **Apply
Embedded Test**. That action is registered only when `build.sh` receives a
manifest; ordinary folder-sync packages continue to use **Apply InkBridge Sync**.
The plugin refuses to apply a baseline-backed manifest when a
different filename is open.

The plugin resolves an existing native stroke by:

1. its Supernote/InkBridge identity;
2. an InkBridge tag from an earlier application;
3. a unique native geometry and style match.

Updates insert the replacement before deleting the superseded native element,
matching the behavior validated on the Nomad. Re-running the same package is
idempotent and reports already-current or already-absent operations as skipped.
An InkBridge tag alone is not treated as proof that a stroke is current: the
plugin also verifies the live native geometry after applying the Supernote
color and coordinate transforms, so lasso edits are not hidden by stale tag
metadata.
The plugin applies all upserts before any explicit deletes so an interrupted
cross-page move cannot remove the only native copy. It scans each affected page
at most once per safety phase and batches the phase's changes, avoiding a full
native-stroke rescan after every operation.

An embedded manifest is a point-in-time, one-way change set. Do not reapply an
old package after editing its imported strokes on Supernote: the package will
correctly reassert the older BOOX state. Export and merge the newer Supernote
state before generating the next return manifest.

The Note Air 4C -> Nomad hardware proof completed 19 operations in about 1.25
seconds after the page scan with the batched implementation. The imported BOOX
stroke remained lassoable, movable, and erasable as native Supernote ink.

## Logs

Use:

```text
INKBRIDGE_SYNC_OP
INKBRIDGE_SYNC_DONE
INKBRIDGE_SYNC_ERROR
```

A successful completion looks like:

```text
INKBRIDGE_SYNC_DONE manifest=... operations=... added=... updated=... deleted=... skipped=...
```

## Current transport boundary

The plugin-preview runtime did not reliably load the earlier arbitrary
filesystem dependency, so this milestone embeds the manifest into a
document-specific `.snplg` package. The converter and importer are transport
independent; a companion app, local service, or supported future plugin file API
can replace this packaging step without changing the manifest or merge logic.

Text-selection highlights remain a separate annotation class and are not part of
the handwriting manifest.
