# InkBridge Supernote native-stroke proofs

This is a deliberately tiny official Supernote plugin proof-of-concept.

## Native Supernote duplication proof

The **InkBridge Test** toolbar action:

1. Reads the currently-open file and current page via `PluginCommAPI`.
2. Enumerates the page's native `Element` objects with `PluginFileAPI.getElements`.
3. Finds the first handwritten stroke.
4. Reads its native EMR points and pressure samples.
5. Creates a new stroke element using `PluginCommAPI.createElement(0)`.
6. Copies the stroke style/pressure while offsetting the geometry by ~80×50 screen pixels.
7. Inserts the new stroke with `PluginFileAPI.insertElements`.
8. Calls `PluginCommAPI.reloadFile`.

Hardware validation on a Nomad confirmed that the inserted copy is lassoable, movable, and erasable like ordinary native Supernote handwriting.

## BOOX → Supernote transfer proof

The **Import BOOX Test** toolbar action inserts a stroke extracted from a real BOOX Note Air 4C NeoReader `#ONYX-STROKE` annotation.

The fixture stores normalized page coordinates plus pressure samples from BOOX `/onyxpoints`. BOOX reports `maxPressure=4095`, while Ratta documents Supernote stroke pressure as `0..4096`, so the proof preserves pressure values directly.

At runtime the plugin:

1. Reads the current Supernote page size.
2. Maps the normalized BOOX page coordinates into Supernote page pixels.
3. Converts those pixels to Supernote EMR coordinates via `PointUtils.androidPoint2Emr`.
4. Creates a native pressure-pen stroke (`penType=16`).
5. Inserts it with `PluginFileAPI.insertElements` and reloads the document.

The acceptance test is that this BOOX-originated stroke appears in native Supernote NOTE/DOC and can immediately be lassoed, moved, and erased.

## Build

The build script scaffolds Ratta's official React Native 0.79.2 plugin template, overlays the InkBridge proof code, and runs the official `buildPlugin.sh` packager:

```bash
cd supernote-poc
./build.sh
```

Output:

```text
supernote-poc/out/*.snplg
```

GitHub Actions also uploads the `.snplg` as the `inkbridge-supernote-poc` artifact.

## Install / update on Supernote

1. Copy the generated `.snplg` to the device's `MyStyle` directory.
2. Open **Settings → Apps → Plugins**.
3. Choose **Add Plugin** / update the existing InkBridge Test plugin.
4. Open a disposable PDF/DOC in the native reader.

Both proof buttons are registered with `showType: 0`, so they run headlessly and leave you in the document.

### Test native duplication

1. Write one ordinary pen stroke on the current page.
2. Tap **InkBridge Test**.
3. Use the native lasso/eraser tools on the offset copied stroke.

### Test real BOOX → Supernote translation

1. Open a disposable PDF/DOC page.
2. Tap **Import BOOX Test**.
3. A BOOX-originated stroke should appear toward the lower-left area of the page.
4. Lasso it, move it, and erase it with the native Supernote tools.

Do not use an important document for these proofs. The plugin intentionally inserts native elements into the current file.
