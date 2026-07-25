# InkBridge Supernote native-stroke proofs

This is a deliberately small official Supernote plugin proof-of-concept that now also supports the first real-document round-trip test.

## Proven hardware paths

### Native Supernote duplication

**InkBridge Test** reads a native Supernote stroke, creates a second native element through the official plugin API, and reloads the document. Hardware validation on a Nomad confirmed the inserted copy is lassoable, movable, and erasable like ordinary native handwriting.

### BOOX → Supernote

**Import BOOX Test** inserts geometry and pressure extracted from a real BOOX Note Air 4C NeoReader `#ONYX-STROKE`. Hardware validation confirmed that the BOOX-originated stroke becomes ordinary editable native Supernote ink.

## Real-document Supernote → BOOX export

**Export Page Test** exports every native handwritten stroke on the currently-open page. For each stroke it preserves:

- Supernote UUID (used as the cross-device identity when present)
- normalized page coordinates
- pressure samples (`0..4096`)
- layer and thickness
- pen color and pen type
- optional `userData`
- source element index as a fallback identity/debugging aid

The page payload also includes the source filename, page index and page pixel size.

For this proof the JSON is emitted to Android logcat as numbered `INKBRIDGE_EXPORT` chunks, followed by an `INKBRIDGE_EXPORT_DONE` summary. This keeps the plugin entirely on the already-proven `sn-plugin-lib` runtime path and avoids the native filesystem dependency that caused plugin 0.0.4 not to load in the document toolbar.

The exported Supernote UUID will be carried into the PDF annotation `/NM` identity. NeoReader previously preserved external `/NM` values while adopting and editing standard PDF `/Ink`, so the returned PDF can be matched back to the original Supernote elements for move/delete/update testing.

## Build

```bash
cd supernote-poc
./build.sh
```

Output:

```text
supernote-poc/out/*.snplg
```

GitHub Actions also uploads the `.snplg` as the `inkbridge-supernote-poc` artifact.

## First real-document round-trip test

Use a **copy** of a real PDF rather than an important original.

### 1. Annotate one real page on Supernote

Open the PDF normally and annotate one page naturally. Use several strokes; moving or erasing something before export is fine.

### 2. Capture the page export

Connect the Nomad over ADB and clear old logs:

```powershell
.\adb logcat -c
```

Then start a live capture before tapping the toolbar action:

```powershell
.\adb logcat -v raw | Select-String INKBRIDGE_EXPORT | Tee-Object InkBridge_Page.log
```

On the Nomad tap **Export Page Test**. After an `INKBRIDGE_EXPORT_DONE` line appears, stop the command with Ctrl+C and keep `InkBridge_Page.log`.

Live capture is preferred to `adb logcat -d` because a heavily annotated real page may generate enough data to roll older chunks out of the log buffer.

### 3. BOOX half of the round trip

Reassemble the numbered chunks into the page JSON and convert each Supernote stroke to a standard PDF `/Ink` annotation using its stable identity. Open that PDF in NeoReader and verify the page looks correct and remains editable.

Then on BOOX:

1. add new handwriting;
2. move at least one Supernote-originated stroke;
3. delete at least one Supernote-originated stroke;
4. use **Embed Data to PDF**.

The returned PDF can then be parsed by stable annotation identity and translated into Supernote insert/modify/delete operations for the final return trip.

The first real-document milestone is deliberately page-at-a-time and manually transferred. Automatic Syncthing/cloud transport and whole-document background synchronization come after this round-trip behavior is proven.
