# InkBridge Runner

InkBridge Runner automates the desktop portion of the native-reader handwriting
round trip while keeping NeoReader and Supernote DOC in place. It supports the
Windows setup used by the Note Air 4C and Nomad hardware proof and uses only
Node.js, ADB, Cargo, Git Bash, and the existing InkBridge build tools.

The official Supernote plugin manager remains the installation boundary. The
runner copies a uniquely versioned, checksum-verified `.snplg` into `MyStyle`;
the user confirms that update in the plugin manager and taps **Apply InkBridge
Sync** in the original document.

## 1. Check both tablets

Connect and authorize both tablets over USB, then run from the repository root:

```powershell
scripts\InkBridge-Runner.cmd doctor
```

The runner recognizes ONYX/BOOX and Supernote/Ratta properties rather than
assuming that ADB returns the devices in a particular order. If two devices of
one type are connected, select them explicitly with `--boox SERIAL` or
`--supernote SERIAL`.

ADB is found from `--adb`, `INKBRIDGE_ADB`, the standard Android SDK location,
or `PATH`, in that order.

On Windows, the runner automatically selects an installed
`stable-x86_64-pc-windows-gnu` toolchain when MSVC build tools are unavailable.
Use `--cargo-toolchain NAME` or `INKBRIDGE_RUST_TOOLCHAIN` to override it.

## 2. Capture a Supernote page baseline

```powershell
scripts\InkBridge-Runner.cmd capture `
  --output C:\InkBridge\MyDocument-page-1.log
```

After the runner clears only the Supernote log buffer, it asks you to open the
target page and tap **Export Page Test**. It waits for every numbered chunk and
the final `INKBRIDGE_EXPORT_DONE` marker before atomically saving the log. A
partial export is never accepted.

Repeat this step for each page that already contains Supernote handwriting and
will participate in the round trip.

## 3. Prepare the BOOX return

After editing in NeoReader and choosing **Embed Data to PDF**, provide the PDF's
shared-storage path on BOOX:

```powershell
scripts\InkBridge-Runner.cmd prepare `
  --boox-pdf "/sdcard/Books/MyDocument.pdf" `
  --baseline C:\InkBridge\MyDocument-page-1.log `
  --output-dir C:\InkBridge\MyDocument-return
```

Use another `--baseline` for each exported page. The runner then:

1. pulls the PDF from the detected BOOX;
2. runs `inkbridge-convert` with all baselines;
3. builds a manifest-specific plugin with a newer version code;
4. computes its SHA-256 hash;
5. copies it to `/sdcard/MyStyle` on the detected Supernote;
6. verifies the on-device SHA-256 hash.

If the PDF is already on the PC, replace `--boox-pdf` with `--pdf LOCAL_PATH`.
Use `--no-push` to build without copying anything to the Nomad.

If a build completed while the Nomad was disconnected, copy it later without
rebuilding:

```powershell
scripts\InkBridge-Runner.cmd push `
  --plugin C:\InkBridge\MyDocument-return\InkBridge-sync-....snplg
```

The output directory retains the pulled PDF, manifest, generated plugin config,
and final `.snplg` so a failed or interrupted attempt can be audited without
repeating earlier steps.

## 4. Confirm and apply on Supernote

On the Nomad:

1. update InkBridge from the new package in `MyStyle`;
2. open the original PDF—not the returned BOOX copy;
3. tap **Apply InkBridge Sync** once;
4. wait for the completion message before editing imported ink.

The runner deliberately does not automate the official plugin-manager
confirmation or the final document mutation.

## Safety checks

- Device roles are verified from manufacturer/model properties.
- Explicit serials cannot silently select the opposite device type.
- Device paths must remain inside shared Android storage and may not traverse
  through `..`.
- Baseline parsing and document-name checks remain enforced by the converter.
- Each generated plugin receives a monotonic Android-safe version code.
- The uploaded plugin must match the local SHA-256 hash.
- The existing Supernote apply path remains idempotent and applies insertions
  before deletions.
