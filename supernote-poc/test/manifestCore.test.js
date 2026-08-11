import assert from 'node:assert/strict';
import test from 'node:test';
import {
  descriptorMatches,
  geometryFingerprint,
  liveSnapshotMatches,
  operationSafetyPhases,
  strokeDescriptor,
  supernotePenColor,
  validateManifest,
} from '../overlay/manifestCore.js';

const style = {
  layerNum: 0,
  thickness: 400,
  penColor: 0,
  penType: 16,
};
const samples = [
  [0.1, 0.2, 1000],
  [0.2, 0.3, 1100],
];

test('geometry fingerprint is stable across Rust and JavaScript', () => {
  assert.equal(geometryFingerprint(style, samples), 'fnv1a32:c1d82be3');
  assert.notEqual(
    geometryFingerprint(style, samples),
    geometryFingerprint({...style, layerNum: 1}, samples),
  );
});

test('descriptor matching tolerates tiny native coordinate round trips', () => {
  const original = strokeDescriptor({nativeStyle: style, samples});
  const roundTripped = strokeDescriptor({
    nativeStyle: style,
    samples: [
      [0.1005, 0.1995, 1000],
      [0.1995, 0.3005, 1100],
    ],
  });
  assert.equal(descriptorMatches(original, roundTripped), true);
});

test('descriptor fallback does not match ink from another native layer', () => {
  const original = strokeDescriptor({nativeStyle: style, samples});
  const otherLayer = strokeDescriptor({
    nativeStyle: {...style, layerNum: 1},
    samples,
  });
  assert.equal(descriptorMatches(original, otherLayer), false);
});

test('manifest validation rejects an unconfigured plugin', () => {
  assert.throws(() => validateManifest(null), /no InkBridge manifest/);
});

test('manifest validation accepts an upsert operation', () => {
  const snapshot = {
    sourceUuid: 'stroke-1',
    origin: 'boox-neoreader',
    pageIndex: 0,
    nativeStyle: style,
    samples,
    geometryFingerprint: geometryFingerprint(style, samples),
  };
  assert.doesNotThrow(() =>
    validateManifest({
      schemaVersion: 1,
      manifestId: 'manifest-1',
      operations: [
        {
          type: 'upsert_stroke',
          sourceUuid: 'stroke-1',
          pageIndex: 0,
          before: null,
          after: snapshot,
        },
      ],
    }),
  );
});

test('unsupported BOOX colors map to a valid native Supernote shade', () => {
  assert.equal(supernotePenColor(0x00), 0x00);
  assert.equal(supernotePenColor(0x9d), 0x9d);
  assert.equal(supernotePenColor(130), 0x9d);
});

test('tagged stroke must retain its transformed live geometry', () => {
  const source = {
    nativeStyle: {...style, penColor: 130},
    samples,
  };
  const current = {
    nativeStyle: {...style, penColor: 0x9d},
    samples: [
      [0.1, 0.1992, 1000],
      [0.2, 0.2992, 1100],
    ],
  };
  assert.equal(liveSnapshotMatches(current, source, -0.0008), true);

  const moved = {
    ...current,
    samples: current.samples.map(([x, y, pressure]) => [
      x + 0.01,
      y,
      pressure,
    ]),
  };
  assert.equal(liveSnapshotMatches(moved, source, -0.0008), false);

  const movedLayer = {
    ...current,
    nativeStyle: {...current.nativeStyle, layerNum: 1},
  };
  assert.equal(liveSnapshotMatches(movedLayer, source, -0.0008), false);
});

test('cross-page destinations are scheduled before explicit source deletions', () => {
  const phases = operationSafetyPhases([
    {type: 'delete_stroke', sourceUuid: 'moved', pageIndex: 0},
    {type: 'upsert_stroke', sourceUuid: 'moved', pageIndex: 1},
    {type: 'upsert_stroke', sourceUuid: 'added', pageIndex: 0},
  ]);

  assert.deepEqual(
    phases.map(phase => phase.map(({operation}) => operation.type)),
    [
      ['upsert_stroke', 'upsert_stroke'],
      ['delete_stroke'],
    ],
  );
  assert.deepEqual(
    phases.flat().map(({index}) => index),
    [1, 2, 0],
  );
});
