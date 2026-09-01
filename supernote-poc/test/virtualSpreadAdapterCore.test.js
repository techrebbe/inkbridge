import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import {fileURLToPath} from 'node:url';
import {
  buildVirtualSpreadSnapshot,
  canonicalPointToSpread,
  manifestToVirtualSpread,
  nativeViewportForVirtualSpread,
  spreadPointToCanonical,
  validateVirtualSpreadRepresentation,
} from '../overlay/virtualSpreadAdapterCore.js';
import {
  fixtureForOpenPath,
  fixtureNativeDescriptor,
  PAGE_143_VIRTUAL_SPREAD_FIXTURE,
} from '../overlay/virtualSpreadFixture.js';
import {normalizedEmrPoint} from '../overlay/emrPointSpaceCore.js';

const fixtureRoot = fileURLToPath(
  new URL('../../inkbridge-convert/tests/fixtures/virtual-spread/page-143-v1/', import.meta.url),
);
const sidecar = JSON.parse(
  readFileSync(`${fixtureRoot}/page-143-virtual-spread-v1.pdf.json`, 'utf8'),
);
const artifacts = JSON.parse(
  readFileSync(`${fixtureRoot}/page-143-artifacts-v1.json`, 'utf8'),
);

function close(left, right, tolerance = 1e-12) {
  assert.ok(
    Math.abs(left - right) <= tolerance,
    `${left} differs from ${right} by more than ${tolerance}`,
  );
}

const style = {layerNum: 0, thickness: 400, penColor: 0, penType: 16};
const nativePageSize = {width: 1872, height: 1404};
function nativeViewport(virtualPageIndex, overrides = {}) {
  return {
    schemaVersion: 1,
    authority: 'rtl-reader-native-viewport-v1',
    documentId: PAGE_143_VIRTUAL_SPREAD_FIXTURE.documentId,
    viewId: PAGE_143_VIRTUAL_SPREAD_FIXTURE.viewId,
    virtualPageIndex,
    nativePageSize: [nativePageSize.width, nativePageSize.height],
    spreadToNative: [
      (nativePageSize.width - 1) /
        PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[0],
      0,
      0,
      -(nativePageSize.height - 1) /
        PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[1],
      0,
      nativePageSize.height - 1,
    ],
    ...overrides,
  };
}
const nativeViewports = new Map([
  [0, nativeViewport(0)],
  [1, nativeViewport(1)],
]);
const nativePageSizes = new Map([
  [0, nativePageSize],
  [1, nativePageSize],
]);

test('embedded hardware-gate descriptor is pinned to the normative real fixture', () => {
  const fixture = validateVirtualSpreadRepresentation(
    PAGE_143_VIRTUAL_SPREAD_FIXTURE,
  );
  assert.equal(fixture.documentId, sidecar.source.documentId);
  assert.equal(fixture.viewId, sidecar.output.viewId);
  assert.equal(fixture.cacheBasename, sidecar.output.cacheBasename);
  assert.equal(fixture.generatedPdfSha256, artifacts.output.sha256);
  assert.equal(fixture.sidecarSha256, artifacts.output.sidecarSha256);
  assert.equal(fixture.mappingAuthoritySha256, sidecar.output.mappingAuthoritySha256);
  assert.equal(fixture.sourceFileName, sidecar.source.name);
  assert.deepEqual(fixture.spreadSize, sidecar.output.spreadSize);
  assert.deepEqual(
    fixture.mappings,
    sidecar.sourcePages.map(mapping => ({
      sourcePageIndex: mapping.sourcePageIndex,
      virtualPageIndex: mapping.virtualPageIndex,
      side: mapping.side,
      sourceRotation: mapping.sourceRotation,
      sourceBox: mapping.sourceBox,
      destination: mapping.destination,
      transform: mapping.transform,
    })),
  );
  assert.equal(
    fixtureForOpenPath(`/storage/cache/${fixture.cacheBasename}`),
    fixture,
  );
  assert.equal(fixtureForOpenPath('/storage/cache/ordinary.pdf'), null);
  assert.deepEqual(JSON.parse(fixtureNativeDescriptor()), {
    schemaVersion: 1,
    documentId: fixture.documentId,
    viewId: fixture.viewId,
    cacheBasename: fixture.cacheBasename,
    generatedPdfSha256: fixture.generatedPdfSha256,
    sidecarSha256: fixture.sidecarSha256,
    mappingAuthoritySha256: fixture.mappingAuthoritySha256,
    sourceFileName: fixture.sourceFileName,
    sourcePageCount: fixture.sourcePageCount,
  });
});

test('page-143 exact point and stroke vectors round trip through the local inverse', () => {
  const mapping = PAGE_143_VIRTUAL_SPREAD_FIXTURE.mappings[2];
  for (const vector of artifacts.pointRoundTrips) {
    const spread = canonicalPointToSpread(mapping, vector.normalized);
    close(spread[0], vector.spread[0]);
    close(spread[1], vector.spread[1]);
    const recovered = spreadPointToCanonical(mapping, vector.spread);
    close(recovered[0], vector.normalizedAfterInverse[0]);
    close(recovered[1], vector.normalizedAfterInverse[1]);
  }
  artifacts.strokeRoundTrip.normalized.forEach((point, index) => {
    const spread = canonicalPointToSpread(mapping, point);
    close(spread[0], artifacts.strokeRoundTrip.spread[index][0]);
    close(spread[1], artifacts.strokeRoundTrip.spread[index][1]);
    const recovered = spreadPointToCanonical(
      mapping,
      artifacts.strokeRoundTrip.spread[index],
    );
    close(recovered[0], artifacts.strokeRoundTrip.normalizedAfterInverse[index][0]);
    close(recovered[1], artifacts.strokeRoundTrip.normalizedAfterInverse[index][1]);
  });
});

test('one native spread scan produces two complete original-page snapshots', () => {
  const samples = artifacts.strokeRoundTrip.spread.map(([x, y], index) => [
    x / PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[0],
    1 - y / PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[1],
    1000 + index,
  ]);
  const pages = buildVirtualSpreadSnapshot({
    representation: PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    virtualPageIndex: 1,
    nativeViewport: nativeViewport(1),
    nativePageSize,
    strokes: [
      {
        sourceUuid: 'page-143-stroke',
        sourceKey: 'page-143-stroke',
        layerNum: 0,
        thickness: 400,
        penColor: 0,
        penType: 16,
        samples,
      },
    ],
  });
  assert.deepEqual(pages.map(page => page.pageIndex), [1, 2]);
  assert.deepEqual(pages[0].strokes, []);
  assert.equal(pages[1].strokes[0].sourceUuid, 'page-143-stroke');
  pages[1].strokes[0].samples.forEach((sample, index) => {
    close(sample[0], artifacts.strokeRoundTrip.normalizedAfterInverse[index][0]);
    close(sample[1], artifacts.strokeRoundTrip.normalizedAfterInverse[index][1]);
    assert.equal(sample[2], 1000 + index);
  });
});

test('the hardware-captured composed EMR range classifies page-143 ink', () => {
  const emrRange = {maxX: 15819, maxY: 21098};
  const samples = [
    {x: 9197, y: 17991},
    {x: 8543, y: 17199},
    {x: 8990, y: 15900},
  ].map((point, index) => [
    ...normalizedEmrPoint(point, emrRange),
    1000 + index,
  ]);
  const pages = buildVirtualSpreadSnapshot({
    representation: PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    virtualPageIndex: 1,
    nativeViewport: nativeViewport(1),
    nativePageSize,
    strokes: [{
      sourceUuid: 'hardware-page-143-stroke',
      sourceKey: 'hardware-page-143-stroke',
      layerNum: 0,
      thickness: 533,
      penColor: 157,
      penType: 10,
      samples,
    }],
  });

  assert.deepEqual(pages.map(page => page.pageIndex), [1, 2]);
  assert.deepEqual(pages[0].strokes, []);
  assert.equal(pages[1].strokes.length, 1);
  assert.equal(
    pages[1].strokes[0].sourceUuid,
    'hardware-page-143-stroke',
  );
});

test('canonical upserts and tombstones target the correct native spread page and half', () => {
  const canonicalSamples = artifacts.strokeRoundTrip.normalized.map(([x, y], index) => [
    x,
    y,
    1200 + index,
  ]);
  const snapshot = {
    sourceUuid: 'gate-stroke',
    origin: 'supernote-native',
    pageIndex: 2,
    nativeStyle: style,
    samples: canonicalSamples,
    geometryFingerprint: 'canonical-fingerprint',
  };
  const transformed = manifestToVirtualSpread(
    {
      schemaVersion: 1,
      manifestId: 'page-143-hardware-gate',
      coordinateTransform: {pdfToSupernoteNormalizedYOffset: -0.0008},
      operations: [
        {
          type: 'upsert_stroke',
          sourceUuid: 'gate-stroke',
          pageIndex: 2,
          before: null,
          after: snapshot,
        },
        {
          type: 'delete_stroke',
          sourceUuid: 'deleted-stroke',
          pageIndex: 2,
          before: snapshot,
          after: null,
        },
      ],
    },
    PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    nativeViewports,
    nativePageSizes,
  );
  assert.equal(transformed.coordinateTransform.pdfToSupernoteNormalizedYOffset, 0);
  assert.deepEqual(transformed.operations.map(operation => operation.pageIndex), [1, 1]);
  const native = transformed.operations[0].after;
  native.samples.forEach((sample, index) => {
    close(
      sample[0],
      artifacts.strokeRoundTrip.spread[index][0] /
        PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[0],
    );
    close(
      sample[1],
      1 -
        artifacts.strokeRoundTrip.spread[index][1] /
          PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[1],
    );
    assert.equal(sample[2], 1200 + index);
  });
  assert.notEqual(native.geometryFingerprint, 'canonical-fingerprint');
  assert.equal(transformed.operations[1].after, null);
  assert.equal(transformed.operations[1].before.pageIndex, 1);
});

test('a cross-half move does not delete its destination when both halves share one native page', () => {
  const snapshot = {
    sourceUuid: 'cross-half',
    origin: 'supernote-native',
    pageIndex: 2,
    nativeStyle: style,
    samples: [
      [0.1, 0.2, 1000],
      [0.2, 0.3, 1100],
    ],
    geometryFingerprint: 'after',
  };
  const transformed = manifestToVirtualSpread(
    {
      schemaVersion: 1,
      manifestId: 'cross-half-move',
      operations: [
        {
          type: 'upsert_stroke',
          sourceUuid: 'cross-half',
          pageIndex: 2,
          before: null,
          after: snapshot,
        },
        {
          type: 'delete_stroke',
          sourceUuid: 'cross-half',
          pageIndex: 1,
          before: {...snapshot, pageIndex: 1, geometryFingerprint: 'before'},
          after: null,
        },
      ],
    },
    PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    nativeViewports,
    nativePageSizes,
  );
  assert.equal(transformed.operations.length, 1);
  assert.equal(transformed.operations[0].type, 'upsert_stroke');
  assert.equal(transformed.operations[0].pageIndex, 1);
  assert.ok(transformed.operations[0].before);
  assert.equal(transformed.operations[0].before.pageIndex, 1);
  const sourceMapping = PAGE_143_VIRTUAL_SPREAD_FIXTURE.mappings.find(
    mapping => mapping.sourcePageIndex === 1,
  );
  const expectedSource = canonicalPointToSpread(sourceMapping, [0.1, 0.2]);
  close(
    transformed.operations[0].before.samples[0][0],
    expectedSource[0] / PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[0],
  );
  close(
    transformed.operations[0].before.samples[0][1],
    1 - expectedSource[1] / PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[1],
  );
  assert.notEqual(
    transformed.operations[0].before.geometryFingerprint,
    'before',
  );
});

test('strokes in the gutter or crossing source halves fail closed', () => {
  const base = {
    sourceUuid: 'bad-stroke',
    sourceKey: 'bad-stroke',
    layerNum: 0,
    thickness: 400,
    penColor: 0,
    penType: 16,
  };
  assert.throws(
    () =>
      buildVirtualSpreadSnapshot({
        representation: PAGE_143_VIRTUAL_SPREAD_FIXTURE,
        virtualPageIndex: 1,
        nativeViewport: nativeViewport(1),
        nativePageSize,
        strokes: [{...base, samples: [[0.25, 0.05, 1000], [0.3, 0.06, 1000]]}],
      }),
    /margin/,
  );
  assert.throws(
    () =>
      buildVirtualSpreadSnapshot({
        representation: PAGE_143_VIRTUAL_SPREAD_FIXTURE,
        virtualPageIndex: 1,
        nativeViewport: nativeViewport(1),
        nativePageSize,
        strokes: [{...base, samples: [[0.25, 0.5, 1000], [0.75, 0.5, 1000]]}],
      }),
    /boundary/,
  );
});

test('native pixel drift at a source-page edge snaps back without changing halves', () => {
  const mapping = PAGE_143_VIRTUAL_SPREAD_FIXTURE.mappings.find(
    candidate => candidate.sourcePageIndex === 2,
  );
  const canonical = [
    [1, 0.2],
    [0.9, 0.3],
  ];
  const nativeSamples = canonical.map(([x, y], index) => {
    const [spreadX, spreadY] = canonicalPointToSpread(mapping, [x, y]);
    return [
      spreadX / PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[0] +
        (index === 0 ? 0.5 / (nativePageSize.width - 1) : 0),
      1 - spreadY / PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[1],
      1000 + index,
    ];
  });
  const pages = buildVirtualSpreadSnapshot({
    representation: PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    virtualPageIndex: 1,
    nativeViewport: nativeViewport(1),
    nativePageSize,
    strokes: [
      {
        sourceUuid: 'edge-stroke',
        sourceKey: 'edge-stroke',
        layerNum: 0,
        thickness: 400,
        penColor: 0,
        penType: 16,
        samples: nativeSamples,
      },
    ],
  });
  const exported = pages.find(page => page.pageIndex === 2).strokes[0];
  assert.equal(exported.sourceUuid, 'edge-stroke');
  close(exported.samples[0][0], 1);
  close(exported.samples[0][1], 0.2);
  close(exported.samples[1][0], 0.9);
  close(exported.samples[1][1], 0.3);
});

test('a stroke wholly on the shared seam remains ambiguous and fails closed', () => {
  const base = {
    sourceUuid: 'seam-stroke',
    sourceKey: 'seam-stroke',
    layerNum: 0,
    thickness: 400,
    penColor: 0,
    penType: 16,
  };
  assert.throws(
    () =>
      buildVirtualSpreadSnapshot({
        representation: PAGE_143_VIRTUAL_SPREAD_FIXTURE,
        virtualPageIndex: 1,
        nativeViewport: nativeViewport(1),
        nativePageSize,
        strokes: [
          {
            ...base,
            samples: [
              [0.5, 0.4, 1000],
              [0.5, 0.5, 1001],
            ],
          },
        ],
      }),
    /ambiguous/,
  );
});

test('presentation fails closed without an authoritative PDF viewport', () => {
  const base = {
    sourceUuid: 'portrait-stroke',
    sourceKey: 'portrait-stroke',
    layerNum: 0,
    thickness: 400,
    penColor: 0,
    penType: 16,
    samples: [
      [0.2, 0.4, 1000],
      [0.3, 0.5, 1001],
    ],
  };
  assert.throws(
    () =>
      buildVirtualSpreadSnapshot({
        representation: PAGE_143_VIRTUAL_SPREAD_FIXTURE,
        virtualPageIndex: 1,
        nativeViewport: null,
        nativePageSize,
        strokes: [base],
      }),
    /authoritative RTL Reader native viewport is required/,
  );

  const snapshot = {
    sourceUuid: 'portrait-import',
    origin: 'boox-neoreader',
    pageIndex: 2,
    nativeStyle: style,
    samples: [
      [0.1, 0.2, 1000],
      [0.2, 0.3, 1001],
    ],
    geometryFingerprint: 'portrait-import',
  };
  assert.throws(
    () =>
      manifestToVirtualSpread(
        {
          schemaVersion: 1,
          manifestId: 'portrait-import',
          operations: [
            {
              type: 'upsert_stroke',
              sourceUuid: snapshot.sourceUuid,
              pageIndex: snapshot.pageIndex,
              before: null,
              after: snapshot,
            },
          ],
        },
        PAGE_143_VIRTUAL_SPREAD_FIXTURE,
        new Map([[1, nativePageSize]]),
        new Map([[1, nativePageSize]]),
      ),
    /authoritative RTL Reader native viewport is required/,
  );
});

test('same-aspect native geometry is not accepted as viewport authority', () => {
  assert.throws(
    () =>
      buildVirtualSpreadSnapshot({
        representation: PAGE_143_VIRTUAL_SPREAD_FIXTURE,
        virtualPageIndex: 1,
        nativeViewport: {
          width: nativePageSize.width,
          height: nativePageSize.height,
        },
        nativePageSize,
        strokes: [
          {
            sourceUuid: 'same-aspect-stroke',
            sourceKey: 'same-aspect-stroke',
            layerNum: 0,
            thickness: 400,
            penColor: 0,
            penType: 16,
            samples: [
              [0.1, 0.3, 1000],
              [0.2, 0.4, 1001],
            ],
          },
        ],
      }),
    /authoritative RTL Reader native viewport is required/,
  );
});

test('viewport authority must match the current native page canvas', () => {
  assert.throws(
    () =>
      nativeViewportForVirtualSpread(
        PAGE_143_VIRTUAL_SPREAD_FIXTURE,
        nativeViewport(1),
        1,
        {width: 1404, height: 1872},
      ),
    /does not match the current native page canvas/,
  );
});

test('accepted viewport cannot place a spread edge outside the native canvas', () => {
  assert.throws(
    () =>
      nativeViewportForVirtualSpread(
        PAGE_143_VIRTUAL_SPREAD_FIXTURE,
        nativeViewport(1, {
          spreadToNative: [
            (nativePageSize.width - 1) /
              PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[0],
            0,
            0,
            -(nativePageSize.height - 1) /
              PAGE_143_VIRTUAL_SPREAD_FIXTURE.spreadSize[1],
            -0.5,
            nativePageSize.height - 1,
          ],
        }),
        1,
        nativePageSize,
      ),
    /lies outside the native page/,
  );
});

test('viewport authority rejects an ill-conditioned affine matrix', () => {
  assert.throws(
    () =>
      nativeViewportForVirtualSpread(
        PAGE_143_VIRTUAL_SPREAD_FIXTURE,
        nativeViewport(1, {
          spreadToNative: [1, 0, 1, 1e-13, 0, 700],
        }),
        1,
        nativePageSize,
      ),
    /numerically unstable/,
  );
});

test('rotated viewport edge tolerance is measured in native pixels', () => {
  const rotatedViewport = nativeViewport(1, {
    spreadToNative: [0.75, 0.75, -0.75, 0.75, 486, 100],
  });
  const spreadSamples = [
    [-1.8, 149.4, 1000],
    [-1.7, 149.5, 1001],
  ];
  const nativeSamples = spreadSamples.map(([x, y, pressure]) => {
    const [a, b, c, d, e, f] = rotatedViewport.spreadToNative;
    return [
      (a * x + c * y + e) / (nativePageSize.width - 1),
      (b * x + d * y + f) / (nativePageSize.height - 1),
      pressure,
    ];
  });
  assert.throws(
    () =>
      buildVirtualSpreadSnapshot({
        representation: PAGE_143_VIRTUAL_SPREAD_FIXTURE,
        virtualPageIndex: 1,
        nativeViewport: rotatedViewport,
        nativePageSize,
        strokes: [
          {
            sourceUuid: 'rotated-margin-stroke',
            sourceKey: 'rotated-margin-stroke',
            layerNum: 0,
            thickness: 400,
            penColor: 0,
            penType: 16,
            samples: nativeSamples,
          },
        ],
      }),
    /margin/,
  );
});
