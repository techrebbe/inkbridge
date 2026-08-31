# InkBridge Supernote folder companion

InkBridge 0.2.2 moves the proven native-stroke export and manifest importer onto the finalized
folder contract. The `.snplg` contains its own small native Android bridge; it does not require a
second companion application, logcat capture, a document-specific plugin package, or changes to the
actively developed RTL Reader project.

## Shared-storage layout

The native bridge derives the stable document ID from the immutable PDF bytes and writes under:

    /storage/emulated/0/EXPORT/InkBridge/Supernote_Folder/
      inkbridge-doc-v1-<original-pdf-sha256>/
        outgoing/page-0001.json
        incoming/r<boox-revision>-r<supernote-revision>-g<generation>-<event>.operations.json
        incoming/inkbridge-status.json
        acknowledged/<delivery-sha256>.ack.json
        failures/<delivery-sha256>.error.json

The PDF hash is recalculated for every user-initiated folder action. A mirroring tool can replace
bytes in place while preserving a path, length, timestamp, and inode, so those metadata fields are
never treated as proof of document identity. Renaming an unchanged PDF therefore does not change
its identity, while replacing its bytes derives a new identity. Every new page export also carries
this stable document ID inside the JSON. The transport uses that identity
instead of requiring the BOOX and Supernote filenames to match; filename-only validation remains
available for older proof exports. Folder-delivered manifests are already bound to the stable ID
and exact open path, so a later Supernote rename does not invalidate a legitimate update.

For the page-143 Virtual Spread hardware gate, the plugin recognizes only the exact authenticated
fixture cache basename. It verifies the generated PDF and sidecar bytes, their mapping authority,
the hidden versioned cache path, and the immutable original document ID before any folder action.
The original PDF identity and filename—not the derived spread hash or cache name—select the folder.
This is deliberately fixture-scoped; generic production cache activation remains disabled.
For export and import, the plugin calls RTL Reader v0.0.26's live viewport provider with the exact
document, view, virtual-page, native-canvas, file-path, and representation-hash evidence. The
native bridge accepts only the expected provider package and release certificate, then validates
the exact canonical descriptor and activation evidence. It never persists the descriptor or
reconstructs it from dimensions; a page-load, restart, or cache mismatch leaves the action
unavailable until RTL Reader publishes a fresh record.
If one incoming manifest touches more than one physical Virtual Spread page, Apply stages the
authorized current page and records durable progress without acknowledging the delivery. The
status tells the user which spread page to open next; the delivery is acknowledged only after all
target pages have been applied. All destination upsert steps precede explicit source deletions,
including moves across different spreads. Progress is bound to the exact cache path and delivery;
a retry after interruption is idempotent and cannot reuse progress from a replacement cache.

Configure `inkbridge-folder-transport` to use that document directory's `outgoing` and `incoming`
paths after the whole document directory, including `acknowledged`, is mirrored to the machine
running the transport. An outgoing Supernote export remains blocked while any delivered manifest
lacks its matching content-hash acknowledgement; this prevents a page snapshot from claiming a
BOOX revision that has only been downloaded, not applied. The local mirroring
mechanism is deliberately independent of the broker and may be USB, Syncthing, a NAS share, or
another private file transport.

## Toolbar actions

- **Export InkBridge** reads every native stroke on the current page and publishes one complete
  page snapshot. On the authenticated Virtual Spread fixture it scans the physical spread once,
  applies the locally-derived inverse transform, and atomically publishes complete snapshots for
  both represented original pages—including an empty half. It writes and fsyncs a temporary part
  file before a same-directory rename. An empty page is a valid export, allowing deletion of the
  final stroke on a page to synchronize. The
  plugin hashes the PDF before collecting native ink and requires the same stable identity when it
  publishes afterward. It refuses to publish if the user switched documents or a mirror replaced
  the same-path PDF during collection. It also refuses to queue a page snapshot while any
  incoming update is waiting for Apply, preventing pre-update ink from being uploaded against the
  newer broker revision. Every snapshot records the exact applied BOOX/Supernote revision pair.
  The transport rejects an older snapshot if a manifest was delivered or applied before upload;
  tapping Export again produces a fresh snapshot at the new frontier.
  Strokes inserted by InkBridge retain the canonical broker UUID stored in their native user data.
  Otherwise, export uses the Supernote element UUID as the stable native fallback without modifying
  the stroke. Unknown third-party `userData` fails closed instead of being overwritten. Virtual
  Spread export must remain read-only: on the tested landscape page, sending an unchanged element
  back through `modifyElements` solely to attach metadata caused the firmware to reinterpret its EMR
  coordinates and physically transform the stroke. If future firmware stops retaining native UUIDs,
  InkBridge must add a separate durable identity ledger with conservative geometry reconciliation;
  it must not restore the destructive metadata-write path.
- **Apply InkBridge Sync** reads the oldest unacknowledged `*.operations.json`, applies its moves,
  insertions, and deletions through the official Supernote element API, reloads the document, and
  only then publishes a durable acknowledgement. The folder transport prefixes delivered files
  with zero-padded broker revisions and generation, so multiple queued updates are applied in
  causal order rather than by arbitrary event ID. Immediately before native stroke changes, the
  plugin rehashes the open PDF and requires it to match the stable document ID carried by the
  delivery; a same-path file replacement therefore fails safely. The acknowledgement records that
  manifest's source revision pair. If mirroring exposes revision N+1 before N, both the local
  transport and plugin leave N+1 untouched and report that the missing predecessor is required.
  For the authenticated Virtual Spread fixture, original-page canonical samples are transformed
  onto the correct physical spread page and half before native application. A move between two
  halves sharing one physical spread replaces the native element without a subsequent delete of
  the new destination. All queue validation and response construction finish before the
  acknowledgement commit, so a
  later malformed incoming file cannot turn an already acknowledged update into a false failure.
- **InkBridge Status** reports `synced`, `pending`, `conflict`, or `error` using the incoming queue,
  durable acknowledgements/failures, and the transport's `inkbridge-status.json` checkpoint. The
  checkpoint includes accepted export hashes and remains `pending` while a downloaded manifest
  lacks a valid acknowledgement, so an older `synced` status cannot hide a newly finalized page or
  an incoming update waiting to be applied.

Each action remains headless until it finishes and then shows a small result dialog. It does not
replace the native document reader.

## Duplicate and crash behavior

The delivery identity is the SHA-256 of the complete incoming manifest, not its filename. A copied
or redelivered manifest with identical bytes is ignored after acknowledgement. If the plugin is
interrupted after native ink is applied but before the acknowledgement is written, the manifest is
offered again; the already-proven geometry/source tags make reapplication idempotent, after which
the acknowledgement is safely recorded.

Incoming manifests are never deleted by the plugin. Failed application writes a retryable error
record but no acknowledgement. Conflict markers stop automatic application and preserve both
device inputs for explicit reconciliation. Failure records whose original invalid delivery was
removed or replaced are retired during the next queue scan, preventing a corrected file from
leaving the document permanently stuck in `error`.

An acknowledgement suppresses a delivery only after its schema, content-hash delivery ID, and
stable document ID have been validated. A truncated or partially mirrored acknowledgement stops
the action with a repairable error instead of silently hiding an unapplied update.

If an earlier revision is malformed, queue processing stops at that file after recording the
error. Newer deltas are never applied out of order; replace or remove the invalid delivery first.
The same fail-closed rule applies when an earlier delivery simply has not arrived yet.

The broker's validated `inkbridge-status.json` conflict checkpoint is checked before the queue.
Even if an older manifest remains downloaded, the plugin will not apply or acknowledge it while
simultaneous edits are unresolved.
A checkpoint that exists but is truncated, malformed, non-regular, or bound to another document
also blocks the action until mirroring finishes or the file is repaired; it is never treated as an
absent checkpoint.

## Packaging boundary

Ratta's public `FileUtils` module can list, copy, rename, and hash shared files, but it cannot read or
write JSON text. The plugin therefore packages `InkBridgeFolderModule` in the official `app.npk`
native-code slot. The build fails closed if that module, its React package registration, the native
APK, or the package metadata is missing.

The native worker hashes PDFs and performs shared-storage I/O off the PluginHost UI thread. The
bridge accepts only accessible PDFs in shared storage, uses a hash-derived directory name, rejects
non-regular incoming files, caps payload sizes, and validates manifest schema and identity before
returning data to JavaScript.

## Validation

    npm test --prefix supernote-poc
    python3 supernote-poc/scripts/check_folder_module.py supernote-poc
    supernote-poc/build.sh
    cargo fmt --all --check
    cargo clippy -p inkbridge-folder-transport --all-targets -- -D warnings
    cargo test -p inkbridge-folder-transport

The package verifier confirms the `.snplg` contains the reviewed bundle, `app.npk`, Android
bytecode for both native folder classes, and the exact `reactPackages`/`nativeCodePackage` fields.
