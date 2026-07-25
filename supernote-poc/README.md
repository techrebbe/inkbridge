# InkBridge Supernote native-stroke proof

This is a deliberately tiny official Supernote plugin proof-of-concept.

## What it proves

The plugin adds an **InkBridge Test** button to native Supernote NOTE/DOC. When pressed, it:

1. Reads the currently-open file and current page via `PluginCommAPI`.
2. Enumerates the page's native `Element` objects with `PluginFileAPI.getElements`.
3. Finds the first handwritten stroke.
4. Reads its native EMR points and pressure samples.
5. Creates a new stroke element using `PluginCommAPI.createElement(0)`.
6. Copies the stroke style/pressure while offsetting the geometry by ~80×50 screen pixels.
7. Inserts the new stroke with `PluginFileAPI.insertElements`.
8. Calls `PluginCommAPI.reloadFile`.

Success means the copied stroke can then be lassoed, moved, erased, and edited exactly like handwriting created normally on the Supernote.

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

## Install on Supernote

1. Copy the generated `.snplg` to the device's `MyStyle` directory.
2. Open **Settings → Apps → Plugins**.
3. Choose **Add Plugin** and install the package.
4. Open a PDF/DOC in the native reader.
5. Write one ordinary pen stroke on the current page.
6. Tap **InkBridge Test** in the toolbar.
7. Close the plugin panel after it reports success.
8. Use the native lasso/eraser tools on the copied stroke.

Do not use an important document for the proof. The plugin intentionally inserts a new native element into the current file.
