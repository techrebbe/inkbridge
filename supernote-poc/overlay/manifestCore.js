export function parseUserData(userData) {
  if (!userData || typeof userData !== 'string') return null;
  try {
    return JSON.parse(userData);
  } catch {
    return null;
  }
}

function fnv1a32(text) {
  let hash = 0x811c9dc5;
  for (let index = 0; index < text.length; index += 1) {
    const code = text.charCodeAt(index);
    // Manifests use an ASCII-only canonical representation.
    hash ^= code & 0xff;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash >>> 0;
}

export function geometryFingerprint(nativeStyle, samples) {
  const prefix =
    `${nativeStyle.thickness}|${nativeStyle.penColor}|${nativeStyle.penType}|`;
  const canonical = samples
    .map(
      ([x, y, pressure]) =>
        `${Math.round(x * 100000)},${Math.round(y * 100000)},${Math.round(pressure)};`,
    )
    .join('');
  return `fnv1a32:${fnv1a32(prefix + canonical)
    .toString(16)
    .padStart(8, '0')}`;
}

export function validateManifest(manifest) {
  if (!manifest || typeof manifest !== 'object') {
    throw new Error(
      'This plugin package has no InkBridge manifest. Build it with a manifest before installing it.',
    );
  }
  if (manifest.schemaVersion !== 1) {
    throw new Error(
      `Unsupported InkBridge manifest schema ${String(manifest.schemaVersion)}.`,
    );
  }
  if (!manifest.manifestId || !Array.isArray(manifest.operations)) {
    throw new Error('InkBridge manifest is missing its identity or operations.');
  }
  for (const [index, operation] of manifest.operations.entries()) {
    if (
      !['upsert_stroke', 'delete_stroke'].includes(operation?.type) ||
      !operation?.sourceUuid ||
      !Number.isInteger(operation?.pageIndex)
    ) {
      throw new Error(`Invalid InkBridge operation at index ${index}.`);
    }
    const snapshot =
      operation.type === 'upsert_stroke' ? operation.after : operation.before;
    if (
      !snapshot?.nativeStyle ||
      !Array.isArray(snapshot?.samples) ||
      snapshot.samples.length < 2
    ) {
      throw new Error(`InkBridge operation ${operation.sourceUuid} has invalid ink data.`);
    }
  }
  return manifest;
}

export function strokeDescriptor(snapshot) {
  const samples = snapshot?.samples ?? [];
  let minX = 1;
  let maxX = 0;
  let minY = 1;
  let maxY = 0;
  for (const [x, y] of samples) {
    minX = Math.min(minX, x);
    maxX = Math.max(maxX, x);
    minY = Math.min(minY, y);
    maxY = Math.max(maxY, y);
  }
  return {
    pointCount: samples.length,
    minX,
    maxX,
    minY,
    maxY,
    nativeStyle: snapshot.nativeStyle,
  };
}

export function descriptorMatches(left, right, tolerance = 0.003) {
  if (!left || !right) return false;
  if (left.pointCount !== right.pointCount) return false;
  if (left.nativeStyle.thickness !== right.nativeStyle.thickness) return false;
  if (left.nativeStyle.penColor !== right.nativeStyle.penColor) return false;
  if (left.nativeStyle.penType !== right.nativeStyle.penType) return false;
  return ['minX', 'maxX', 'minY', 'maxY'].every(
    key => Math.abs(left[key] - right[key]) <= tolerance,
  );
}
