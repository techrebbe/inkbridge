# InkBridge native bridge proof

## Goal

Keep each vendor's native reading/annotation environment and make the two systems exchange editable ink rather than replacing NeoReader or Supernote DOC with a common reader.

## BOOX proof result (Note Air 4C, 2026-07-24)

A test PDF containing four externally-created standard PDF `/Ink` annotations was opened in NeoReader, edited, and saved with **Embed Data to PDF**.

Inspection of the returned PDF shows:

- Externally-created standard `/Ink` annotations are editable in NeoReader.
- NeoReader preserves their `/NM` annotation IDs and adds `/onyxtag` metadata with a BOOX UUID and `type: PencilStroke`.
- NeoReader can transform those imported `/Ink` annotations (for example scaling/translating them) while leaving them as standard `/Ink` objects.
- Deleting an imported stroke removes it from the page's active annotation list.
- Native NeoReader handwriting embedded into the PDF is stored as `/Subtype /Stamp`, `/Name /#ONYX-STROKE`, with `/onyxtag` `type: BrushStroke` and a stable UUID.
- Native BOOX strokes also carry an `/onyxpoints` stream. The observed stream is structured binary rather than a raster-only appearance: an 8-byte header followed by fixed five-float records. X/Y and elapsed-time fields are directly visible; pressure-like sample values are also present. The remaining per-sample field must be mapped before relying on it.

This is sufficient evidence that BOOX-to-bridge extraction can be implemented without replacing NeoReader.

## Supernote official API capability

Ratta's official plugin API exposes native NOTE/DOC elements, including handwritten strokes with:

- UUID
- page/layer
- thickness
- pen type/color
- EMR points
- pressure samples

The API supports reading, inserting, modifying, replacing, and deleting elements, plus reloading the currently-open file.

Relevant official documentation:

- https://github.com/Supernote-Ratta/docs-plugin
- `PluginFileAPI.getElements`
- `PluginFileAPI.insertElements`
- `PluginFileAPI.modifyElements`
- `PluginCommAPI.reloadFile`

## Supernote proof result (Nomad, 2026-07-25)

The `InkBridge Test` official Supernote plugin duplicated an existing handwritten stroke by:

1. Reading the currently open file/page.
2. Reading an existing native stroke's EMR points and pressure samples.
3. Creating a new stroke element through the official plugin API.
4. Writing offset geometry/pressure data into the new element.
5. Inserting it with `PluginFileAPI.insertElements`.
6. Reloading the current document.

Hardware validation on the Nomad confirmed that the plugin-created duplicate is handled as ordinary native Supernote ink:

- Native lasso selects it.
- Native move transforms it.
- Native eraser deletes/edits it.

Therefore the Supernote half of the native editable-ink bridge is proven.

## Proven architecture

Both devices can now consume externally-created ink while retaining native editability:

- **BOOX:** NeoReader adopts standard external PDF `/Ink` annotations as editable annotations.
- **Supernote:** the official plugin API can insert strokes that behave as normal native Supernote handwriting.

InkBridge should therefore become a lightweight translation/synchronization layer rather than a replacement reader:

- BOOX side: NeoReader remains the reader. A bridge extracts/merges standard `/Ink` plus BOOX `/onyxtag` + `/onyxpoints` annotations.
- Supernote side: the official plugin remains inside native NOTE/DOC and translates native `Element/Stroke` data.
- Portable identity/journal: small InkBridge sidecar for stable cross-device IDs, tombstones, origin metadata, and conflict resolution.
- PDF remains the document carrier/interoperability surface, but not the sole multiwriter conflict database.

## Full round-trip hardware result

The full handwriting round trip passed on a Note Air 4C and Nomad:

- Supernote strokes arrived in NeoReader as editable PDF ink.
- NeoReader moved and deleted Supernote-originated strokes while preserving
  their identities.
- Native BOOX handwriting returned as native Supernote strokes.
- The returned handwriting remained lassoable, movable, and erasable.
- NeoReader vector appearance streams, rather than the raw `onyxpoints` record,
  reproduced the exact rendered BOOX centerline.

The next implementation is no longer a document-specific proof. See
[`INKBRIDGE-MANIFEST-WORKFLOW.md`](INKBRIDGE-MANIFEST-WORKFLOW.md) for the
generic operation manifest and Supernote apply path.

The existing Inkread-based BOOX reader work remains a useful fallback and SDK reference. PR #2 should stay draft while the native-reader bridge becomes the primary architecture.
