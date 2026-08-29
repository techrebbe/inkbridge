#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MANIFEST_PATH="${1:-}"
OUTPUT_DIR="${INKBRIDGE_SN_OUTPUT_DIR:-$ROOT/out}"
WORK_ROOT="$(mktemp -d)"
trap 'rm -rf "$WORK_ROOT"' EXIT

if [[ -n "${PYTHON_BIN:-}" ]]; then
  if command -v cygpath >/dev/null 2>&1 && [[ "$PYTHON_BIN" =~ ^[A-Za-z]:[\\/] ]]; then
    PYTHON_BIN="$(cygpath -u "$PYTHON_BIN")"
  fi
  PYTHON_CMD=("$PYTHON_BIN")
elif command -v python3 >/dev/null 2>&1; then
  PYTHON_CMD=(python3)
elif command -v python >/dev/null 2>&1; then
  PYTHON_CMD=(python)
else
  echo "Python 3 is required to build the native InkBridge plugin." >&2
  exit 1
fi

"${PYTHON_CMD[@]}" "$ROOT/scripts/check_folder_module.py" "$ROOT"

pushd "$WORK_ROOT" >/dev/null
npx --yes @react-native-community/cli@18.0.0 init InkBridgePoc \
  --template @supernote-plugin/sn-plugin-template \
  --version 0.79.2
popd >/dev/null

PROJECT="$WORK_ROOT/InkBridgePoc"
cp "$ROOT/overlay/App.js" "$PROJECT/App.js"
cp "$ROOT/overlay/index.js" "$PROJECT/index.js"
cp "$ROOT/overlay/app.json" "$PROJECT/app.json"
cp "$ROOT/overlay/booxFixture.js" "$PROJECT/booxFixture.js"
cp "$ROOT/overlay/booxReturnFixture.js" "$PROJECT/booxReturnFixture.js"
cp "$ROOT/overlay/booxReturnFixtureV3.js" "$PROJECT/booxReturnFixtureV3.js"
cp "$ROOT/overlay/booxReturnFixtureV4.js" "$PROJECT/booxReturnFixtureV4.js"
cp "$ROOT/overlay/returnApplyV2.js" "$PROJECT/returnApplyV2.js"
cp "$ROOT/overlay/manifestCore.js" "$PROJECT/manifestCore.js"
cp "$ROOT/overlay/manifestApply.js" "$PROJECT/manifestApply.js"
cp "$ROOT/overlay/folderCompanionCore.js" "$PROJECT/folderCompanionCore.js"
cp "$ROOT/overlay/folderCompanion.js" "$PROJECT/folderCompanion.js"
cp "$ROOT/overlay/virtualSpreadAdapterCore.js" "$PROJECT/virtualSpreadAdapterCore.js"
cp "$ROOT/overlay/nativeViewportProviderCore.js" "$PROJECT/nativeViewportProviderCore.js"
cp "$ROOT/overlay/virtualSpreadFixture.js" "$PROJECT/virtualSpreadFixture.js"
if [[ -n "$MANIFEST_PATH" ]]; then
  node "$ROOT/embed-manifest.mjs" "$MANIFEST_PATH" "$PROJECT/generatedManifest.js"
else
  cp "$ROOT/overlay/generatedManifest.js" "$PROJECT/generatedManifest.js"
fi
cp "$ROOT/PluginConfig.json" "$PROJECT/PluginConfig.json"

"${PYTHON_CMD[@]}" "$ROOT/scripts/install_native.py" "$PROJECT" "$ROOT"
"${PYTHON_CMD[@]}" "$ROOT/scripts/patch_plugin_packager.py" "$PROJECT/buildPlugin.sh"

mkdir -p "$PROJECT/assets"
cat > "$PROJECT/assets/icon.png.b64" <<'B64'
iVBORw0KGgoAAAANSUhEUgAAAGAAAABgCAYAAADimHc4AAACbklEQVR4nO2dy3LDIBAEW6n8/y+TgysHO7YEYsXsRtN3yzAt3qVia61hdHypC3B3LECMBYixADEWIMYCxFiAmO+Ih2zbdtvFRGttm/n9NrMQu3Pwr5wVcUqAg//MqIjhMcDh7zOaz5AAh9/HSE7dAhz+GL15hcyCAO64q7ptUxOgxzN6gtuzecfgX9kTcTQoTy3EHP6DmRwOBbjvn+Mov9MtwG//M2fz8F6QGAsQYwFiLECMBYixADEWIMYCxFiAGAsQYwFiLEBM2IHML+/2xitv3F1dn9AW8OlgIuLkSMGK+oQJOCpUNQmr6hMioLcwVSSsrI8HYTEWIMYCxFiAmBABlef5M0TUO6wF9BYm+0yot3xRL11oF1RdwurwYdEY8K7A2SSotlCWDcKZJSj3r5bOgjJKUG8eLp+GZpKgDh9E64AMEjKED8KFmFJClvBBvBJWSMgUPiTYilgpIVv4kEAArJGQMXxIIgCulZA1fEgkAK6RkDl8SCYAYiVkDx8SCoAYCRXCh6QCYE5ClfAhsQA4J6FS+JBcAIxJqBY+FBAAfRIqhg9FBMC+hKrhQyEB0N8dVQkfigmA43ArhQ8FBcDnkKuFDxd8H7AqhNbaU/dz1f9eXZ9wASup+Ma/UrIL+k9YgBgLEGMBYixAjAWIsQAxFiDGAsRYgBgLEGMBYixAjAWIOS1A/WlRNs7mcShg9p6su3PpBQ5uBQ9mcui+R+zoIoL/cDo1ylHwPb1H2JGkW8M5ursgjwVj9OY1NAZYQh8jOQ0Pwpawz2g+vk01iKW3qf55yI1FSO8TNvN4L0iMBYixADEWIMYCxFiAmB+dpA7CEfRVbgAAAABJRU5ErkJggg==
B64
base64 --decode "$PROJECT/assets/icon.png.b64" > "$PROJECT/assets/icon.png"
rm "$PROJECT/assets/icon.png.b64"

pushd "$PROJECT" >/dev/null
chmod +x buildPlugin.sh
# Git Bash otherwise rewrites the package's root-relative payload paths to
# Windows installation paths. Exclude only those two archive-relative values
# so real temp paths still reach the Windows-hosted Node/JQ tools correctly.
MSYS2_ARG_CONV_EXCL='/icon.png;/app.npk' ./buildPlugin.sh
popd >/dev/null

mkdir -p "$OUTPUT_DIR"
rm -f "$OUTPUT_DIR"/*.snplg
cp "$PROJECT"/build/outputs/*.snplg "$OUTPUT_DIR/"

mapfile -t PACKAGES < <(find "$OUTPUT_DIR" -maxdepth 1 -type f -name '*.snplg' -print)
if [[ "${#PACKAGES[@]}" -ne 1 ]]; then
  echo "Expected exactly one generated .snplg, found ${#PACKAGES[@]}." >&2
  exit 1
fi
"${PYTHON_CMD[@]}" "$ROOT/scripts/verify_plugin_package.py" "${PACKAGES[0]}" "$ROOT"

echo "Built Supernote plugin:"
ls -lh "$OUTPUT_DIR"/*.snplg
