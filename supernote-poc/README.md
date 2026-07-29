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

The exported Supernote UUID is carried into the PDF annotation `/NM` identity. NeoReader preserved those values while editing imported `/Ink`, allowing the returned PDF to be matched back to the original Supernote elements.

## Real-document BOOX → Supernote return

Plugin **0.0.6** adds **Apply BOOX Return Test** for the first returned real document. The embedded fixture was extracted from the BOOX PDF after the user:

- moved the original long Supernote line;
- deleted the original gray underline;
- added seven new NeoReader-native handwriting strokes;
- embedded NeoReader data back into the PDF.

The action is intentionally specific to page 1 of the original annotated PDF. It:

1. finds the moved original by its preserved Supernote UUID and updates its native point/pressure accessors;
2. deletes the missing original underline by its native `numInPage`;
3. inserts the seven new BOOX strokes as native Supernote pressure-pen elements;
4. tags inserted strokes with BOOX source IDs so repeating the action does not duplicate them;
5. applies the initial small vertical calibration correction found during the BOOX visual check;
6. reloads the document once all operations finish.

Text-selection highlights are not part of this ink proof. Supernote stores them outside the documented handwritten `Element` stream, so they require a separate annotation adapter.

### Repair build 0.0.10

Version 0.0.10 corrects the two geometry sources independently:

- The moved Supernote-originated line is restored from NeoReader's authoritative standard PDF `/InkList`. Version 0.0.9 incorrectly replaced it with a BOOX-native point stream that described a diagonal spanning most of the page.
- New NeoReader handwriting uses the readable native `/onyxpoints` centerline, with four low-pressure terminal pen-up outliers removed. Those points created visible tails on Supernote that NeoReader does not show.
- The moved line is replaced atomically instead of shortening its point accessor in place. This guarantees that the 207 surplus points written by version 0.0.9 cannot remain at the end of the repaired stroke.
- Revision tags make the repair safe to inspect after one completed run and prevent the corrected BOOX strokes from being duplicated.

### Repair build 0.0.11

Version 0.0.11 uses the exact vector centerline NeoReader wrote into each
native BOOX stroke's PDF `/AP` appearance stream:

- The seven letters now retain all 1,044 NeoReader-rendered samples instead of
  the 251 over-decimated raw points used by 0.0.10.
- Each appearance segment's variable width is converted back into a pressure
  sample so the native Supernote stroke keeps the visible BOOX weight changes.
- The already-correct revision-3 long line is left untouched.
- Only the seven revision-3 BOOX additions are replaced, making this safe to
  run once on the page already repaired by 0.0.10.

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

### 4. Apply the BOOX return on Supernote

1. Install/update InkBridge Test to **0.0.6**.
2. Open page 1 of the original Supernote-annotated PDF copy—not the returned BOOX PDF.
3. Tap **Apply BOOX Return Test** once.
4. Verify the long line moved, the gray underline disappeared, the BOOX handwriting appeared, and the unchanged word was not duplicated.
5. Verify all resulting handwriting remains lassoable, movable and erasable.

The first real-document milestone is deliberately page-at-a-time and manually transferred. Automatic Syncthing/cloud transport and whole-document background synchronization come after this round-trip behavior is proven.
