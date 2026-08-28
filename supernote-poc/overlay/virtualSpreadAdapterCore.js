import {geometryFingerprint} from './manifestCore.js';

const EDGE_TOLERANCE = 1e-9;
const NATIVE_PIXEL_TOLERANCE = 1.5;
const ROUND_TRIP_TOLERANCE = 1e-12;

function finite(value, label) {
  if (!Number.isFinite(value)) throw new Error(`${label} is not finite.`);
  return value;
}

function bounded(value, minimum, maximum, label, tolerance = EDGE_TOLERANCE) {
  finite(value, label);
  if (value < minimum - tolerance || value > maximum + tolerance) {
    throw new Error(`${label} is outside [${minimum}, ${maximum}].`);
  }
  return Math.max(minimum, Math.min(maximum, value));
}

function requireTuple(value, length, label) {
  if (!Array.isArray(value) || value.length !== length) {
    throw new Error(`${label} must contain ${length} numbers.`);
  }
  value.forEach((entry, index) => finite(entry, `${label}[${index}]`));
  return value;
}

function validateMapping(mapping, spreadSize) {
  if (
    !Number.isInteger(mapping?.sourcePageIndex) ||
    mapping.sourcePageIndex < 0 ||
    mapping.sourcePageIndex > 0x7fffffff
  ) {
    throw new Error('Virtual Spread sourcePageIndex is invalid.');
  }
  if (
    !Number.isInteger(mapping.virtualPageIndex) ||
    mapping.virtualPageIndex < 0 ||
    mapping.virtualPageIndex > 0x7fffffff
  ) {
    throw new Error('Virtual Spread virtualPageIndex is invalid.');
  }
  if (!['left', 'right'].includes(mapping.side)) {
    throw new Error('Virtual Spread side is invalid.');
  }
  if (![0, 90, 180, 270].includes(mapping.sourceRotation)) {
    throw new Error('Virtual Spread sourceRotation is invalid.');
  }
  const [left, bottom, right, top] = requireTuple(
    mapping.sourceBox,
    4,
    'sourceBox',
  );
  if (!(right > left && top > bottom)) throw new Error('sourceBox is empty.');
  const [x0, y0, x1, y1] = requireTuple(
    mapping.destination,
    4,
    'destination',
  );
  if (
    !(x1 > x0 && y1 > y0) ||
    x0 < 0 ||
    y0 < 0 ||
    x1 > spreadSize[0] ||
    y1 > spreadSize[1]
  ) {
    throw new Error('Virtual Spread destination is outside the spread.');
  }
  const [a, b, c, d] = requireTuple(mapping.transform, 6, 'transform');
  const determinant = a * d - b * c;
  if (!Number.isFinite(determinant) || determinant <= 0) {
    throw new Error('Virtual Spread transform is reflected or singular.');
  }
  return mapping;
}

export function validateVirtualSpreadRepresentation(representation) {
  if (!representation || representation.schemaVersion !== 1) {
    throw new Error('Unsupported Virtual Spread adapter representation.');
  }
  if (!/^inkbridge-doc-v1-[0-9a-f]{64}$/.test(representation.documentId)) {
    throw new Error('Virtual Spread document identity is invalid.');
  }
  if (!/^inkbridge-view-v1-[0-9a-f]{64}$/.test(representation.viewId)) {
    throw new Error('Virtual Spread view identity is invalid.');
  }
  requireTuple(representation.spreadSize, 2, 'spreadSize');
  if (representation.spreadSize.some(value => value <= 0)) {
    throw new Error('Virtual Spread dimensions must be positive.');
  }
  if (!Array.isArray(representation.mappings) || !representation.mappings.length) {
    throw new Error('Virtual Spread representation has no mappings.');
  }
  const sourcePages = new Set();
  for (const mapping of representation.mappings) {
    validateMapping(mapping, representation.spreadSize);
    if (sourcePages.has(mapping.sourcePageIndex)) {
      throw new Error('Virtual Spread representation repeats a source page.');
    }
    sourcePages.add(mapping.sourcePageIndex);
  }
  return representation;
}

export function mappingsForVirtualPage(representation, virtualPageIndex) {
  validateVirtualSpreadRepresentation(representation);
  if (!Number.isInteger(virtualPageIndex) || virtualPageIndex < 0) {
    throw new Error('Current Virtual Spread page is invalid.');
  }
  const mappings = representation.mappings
    .filter(mapping => mapping.virtualPageIndex === virtualPageIndex)
    .sort((left, right) => left.sourcePageIndex - right.sourcePageIndex);
  if (!mappings.length) {
    throw new Error(`Virtual Spread page ${virtualPageIndex + 1} has no source mappings.`);
  }
  return mappings;
}

function applyTransform(transform, [x, y]) {
  const [a, b, c, d, e, f] = transform;
  return [a * x + c * y + e, b * x + d * y + f];
}

function inverseTransform(transform, [x, y]) {
  const [a, b, c, d, e, f] = transform;
  const determinant = a * d - b * c;
  if (!Number.isFinite(determinant) || determinant <= 0) {
    throw new Error('Virtual Spread inverse is unavailable.');
  }
  const shiftedX = x - e;
  const shiftedY = y - f;
  const result = [
    (d * shiftedX - c * shiftedY) / determinant,
    (-b * shiftedX + a * shiftedY) / determinant,
  ];
  result.forEach((value, index) => finite(value, `inverse[${index}]`));
  return result;
}

function canonicalToOriginal(mapping, [x, y]) {
  const [left, bottom, right, top] = mapping.sourceBox;
  const width = right - left;
  const height = top - bottom;
  switch (mapping.sourceRotation) {
    case 0:
      return [left + x * width, top - y * height];
    case 90:
      return [left + y * width, bottom + x * height];
    case 180:
      return [right - x * width, bottom + y * height];
    case 270:
      return [right - y * width, top - x * height];
    default:
      throw new Error('Unsupported Virtual Spread source rotation.');
  }
}

function originalToCanonical(mapping, [x, y]) {
  const [left, bottom, right, top] = mapping.sourceBox;
  const width = right - left;
  const height = top - bottom;
  switch (mapping.sourceRotation) {
    case 0:
      return [(x - left) / width, (top - y) / height];
    case 90:
      return [(y - bottom) / height, (x - left) / width];
    case 180:
      return [(right - x) / width, (y - bottom) / height];
    case 270:
      return [(top - y) / height, (right - x) / width];
    default:
      throw new Error('Unsupported Virtual Spread source rotation.');
  }
}

export function canonicalPointToSpread(mapping, normalized) {
  const canonical = [
    bounded(normalized[0], 0, 1, 'canonical x'),
    bounded(normalized[1], 0, 1, 'canonical y'),
  ];
  return applyTransform(mapping.transform, canonicalToOriginal(mapping, canonical));
}

export function spreadPointToCanonical(mapping, spread) {
  const canonical = originalToCanonical(
    mapping,
    inverseTransform(mapping.transform, spread),
  );
  const boundedCanonical = [
    bounded(canonical[0], 0, 1, 'recovered canonical x'),
    bounded(canonical[1], 0, 1, 'recovered canonical y'),
  ];
  const reproduced = canonicalPointToSpread(mapping, boundedCanonical);
  const scale = Math.max(1, Math.abs(spread[0]), Math.abs(spread[1]));
  if (
    Math.abs(reproduced[0] - spread[0]) > ROUND_TRIP_TOLERANCE * scale ||
    Math.abs(reproduced[1] - spread[1]) > ROUND_TRIP_TOLERANCE * scale
  ) {
    throw new Error('Virtual Spread inverse round trip is unstable.');
  }
  return boundedCanonical;
}

function nativeSpreadTolerance(representation, nativePageSize) {
  const width = finite(nativePageSize?.width, 'native page width');
  const height = finite(nativePageSize?.height, 'native page height');
  if (width <= 1 || height <= 1) {
    throw new Error('Native Virtual Spread page dimensions are invalid.');
  }
  return [
    (NATIVE_PIXEL_TOLERANCE * representation.spreadSize[0]) / (width - 1),
    (NATIVE_PIXEL_TOLERANCE * representation.spreadSize[1]) / (height - 1),
  ];
}

function pointInsideDestination(mapping, [x, y], [xTolerance, yTolerance]) {
  const [left, top, right, bottom] = mapping.destination;
  return (
    x >= left - xTolerance &&
    x <= right + xTolerance &&
    y >= top - yTolerance &&
    y <= bottom + yTolerance
  );
}

function snapToDestination(mapping, [x, y], [xTolerance, yTolerance]) {
  const [left, top, right, bottom] = mapping.destination;
  const snap = (value, first, second, tolerance) => {
    const firstDistance = Math.abs(value - first);
    const secondDistance = Math.abs(value - second);
    if (firstDistance <= tolerance && firstDistance <= secondDistance) return first;
    if (secondDistance <= tolerance) return second;
    return value;
  };
  return [
    snap(x, left, right, xTolerance),
    snap(y, top, bottom, yTolerance),
  ];
}

export function exportVirtualSpreadStroke(
  stroke,
  representation,
  virtualPageIndex,
  nativePageSize,
) {
  const mappings = mappingsForVirtualPage(representation, virtualPageIndex);
  const spreadTolerance = nativeSpreadTolerance(representation, nativePageSize);
  if (!Array.isArray(stroke?.samples) || stroke.samples.length < 2) {
    throw new Error('Native Virtual Spread stroke has fewer than two samples.');
  }
  const spreadSamples = stroke.samples.map(([x, y, pressure], index) => [
    bounded(x, 0, 1, `native sample ${index} x`) * representation.spreadSize[0],
    // Supernote's Android/EMR boundary is top-left with Y increasing down,
    // while the authenticated PDF transform is bottom-left with Y increasing
    // up.  Keep that convention change at this single native boundary.
    (1 - bounded(y, 0, 1, `native sample ${index} y`)) *
      representation.spreadSize[1],
    bounded(pressure, 0, 4096, `native sample ${index} pressure`, 0),
  ]);
  const candidates = mappings.filter(mapping =>
    spreadSamples.every(sample =>
      pointInsideDestination(mapping, sample, spreadTolerance),
    ),
  );
  if (candidates.length !== 1) {
    throw new Error(
      candidates.length
        ? 'Native stroke is ambiguous across Virtual Spread source pages.'
        : 'Native stroke crosses a Virtual Spread page boundary or lies in the spread margin.',
    );
  }
  const mapping = candidates[0];
  return {
    pageIndex: mapping.sourcePageIndex,
    stroke: {
      ...stroke,
      samples: spreadSamples.map(([x, y, pressure]) => {
        // Native EMR <-> Android conversion can move an edge sample by about
        // one device pixel. Classification uses a page-resolution-derived
        // tolerance, then snaps only those accepted edge samples back onto the
        // authenticated destination before applying the strict inverse.
        const snapped = snapToDestination(
          mapping,
          [x, y],
          spreadTolerance,
        );
        const [canonicalX, canonicalY] = spreadPointToCanonical(mapping, snapped);
        return [canonicalX, canonicalY, pressure];
      }),
    },
  };
}

export function buildVirtualSpreadSnapshot({
  representation,
  virtualPageIndex,
  nativePageSize,
  strokes,
}) {
  const mappings = mappingsForVirtualPage(representation, virtualPageIndex);
  const pages = mappings.map(mapping => ({
    pageIndex: mapping.sourcePageIndex,
    strokes: [],
  }));
  const pagesByIndex = new Map(pages.map(page => [page.pageIndex, page]));
  const identities = new Set();
  for (const stroke of strokes) {
    const exported = exportVirtualSpreadStroke(
      stroke,
      representation,
      virtualPageIndex,
      nativePageSize,
    );
    const identity = exported.stroke.sourceUuid || exported.stroke.sourceKey;
    if (!identity || identities.has(identity)) {
      throw new Error('Virtual Spread snapshot contains a missing or duplicate stroke identity.');
    }
    identities.add(identity);
    pagesByIndex.get(exported.pageIndex).strokes.push(exported.stroke);
  }
  return pages;
}

function mappingForSourcePage(representation, sourcePageIndex) {
  const mapping = representation.mappings.find(
    candidate => candidate.sourcePageIndex === sourcePageIndex,
  );
  if (!mapping) {
    throw new Error(`No Virtual Spread mapping exists for source page ${sourcePageIndex + 1}.`);
  }
  return mapping;
}

function snapshotToVirtual(snapshot, representation) {
  if (!snapshot) return null;
  const mapping = mappingForSourcePage(representation, snapshot.pageIndex);
  const [spreadWidth, spreadHeight] = representation.spreadSize;
  if (!Array.isArray(snapshot.samples) || snapshot.samples.length < 2) {
    throw new Error('Canonical stroke has fewer than two samples.');
  }
  const samples = snapshot.samples.map(([x, y, pressure], index) => {
    requireTuple([x, y, pressure], 3, `canonical sample ${index}`);
    const spread = canonicalPointToSpread(mapping, [x, y]);
    return [
      bounded(spread[0] / spreadWidth, 0, 1, 'virtual sample x'),
      bounded((spreadHeight - spread[1]) / spreadHeight, 0, 1, 'virtual sample y'),
      bounded(pressure, 0, 4096, 'virtual sample pressure', 0),
    ];
  });
  const transformed = {
    ...snapshot,
    pageIndex: mapping.virtualPageIndex,
    samples,
  };
  transformed.geometryFingerprint = geometryFingerprint(
    transformed.nativeStyle,
    transformed.samples,
  );
  return transformed;
}

export function manifestToVirtualSpread(inputManifest, representation) {
  validateVirtualSpreadRepresentation(representation);
  if (!inputManifest || !Array.isArray(inputManifest.operations)) {
    throw new Error('InkBridge manifest has no operations to transform.');
  }
  const transformed = inputManifest.operations.map(operation => {
    const mapping = mappingForSourcePage(representation, operation.pageIndex);
    return {
      originalPageIndex: operation.pageIndex,
      ...operation,
      pageIndex: mapping.virtualPageIndex,
      before: snapshotToVirtual(operation.before, representation),
      after: snapshotToVirtual(operation.after, representation),
    };
  });
  const upserts = new Map(
    transformed
      .filter(operation => operation.type !== 'delete_stroke')
      .map(operation => [operation.sourceUuid, operation]),
  );
  // A canonical cross-page move can arrive as an insertion on the destination
  // page plus a deletion on the source page.  When both source pages occupy
  // one physical Virtual Spread page, applying the explicit delete after the
  // insertion would delete the newly inserted destination as well.  Collapse
  // the pair, but retain the source geometry on the upsert so an interrupted
  // insert-before-delete can find and retire the superseded native stroke on
  // retry.
  for (const operation of transformed) {
    if (operation.type !== 'delete_stroke') continue;
    const destination = upserts.get(operation.sourceUuid);
    if (
      destination &&
      destination.pageIndex === operation.pageIndex &&
      !destination.before
    ) {
      destination.before = operation.before;
    }
  }
  const operations = transformed
    .filter(operation => {
      if (operation.type !== 'delete_stroke') return true;
      const destination = upserts.get(operation.sourceUuid);
      return !destination || destination.pageIndex !== operation.pageIndex;
    })
    .map(({originalPageIndex: _originalPageIndex, ...operation}) => operation);
  return {
    ...inputManifest,
    coordinateTransform: {
      ...(inputManifest.coordinateTransform ?? {}),
      // Virtual Spread coordinates already come from the authenticated affine
      // mapping.  The legacy ordinary-PDF calibration would otherwise shift
      // every imported stroke a second time.
      pdfToSupernoteNormalizedYOffset: 0,
    },
    operations,
  };
}
