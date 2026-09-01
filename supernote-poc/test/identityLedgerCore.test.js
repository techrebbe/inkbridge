import assert from 'node:assert/strict';
import test from 'node:test';
import {reconcileStableStrokeIdentities} from '../overlay/identityLedgerCore.js';

const documentId = `inkbridge-doc-v1-${'a'.repeat(64)}`;
const style = {layerNum: 0, thickness: 532, penColor: 157, penType: 10};

function stroke(sourceUuid, samples, extra = {}) {
  return {
    sourceUuid,
    sourceKey: sourceUuid,
    nativeElementUuid: sourceUuid,
    ...style,
    samples,
    ...extra,
  };
}

function payload(pages) {
  return {schemaVersion: 1, sourceFileName: 'fixture.pdf', pages};
}

function emptyState(bootstrapExports = [], ledger = null) {
  return {schemaVersion: 1, documentId, ledger, bootstrapExports};
}

const original = [
  [0.1, 0.2, 900],
  [0.2, 0.3, 1000],
  [0.3, 0.25, 1100],
];

test('changed native UUID is reconciled from a pending export without modifying ink', () => {
  const prior = payload([
    {pageIndex: 1, strokes: []},
    {pageIndex: 2, strokes: [stroke('stable-uuid', original)]},
  ]);
  prior.documentId = documentId;
  const current = payload([
    {pageIndex: 1, strokes: []},
    {pageIndex: 2, strokes: [stroke('changed-native-uuid', original)]},
  ]);
  const reconciled = reconcileStableStrokeIdentities(
    documentId,
    current,
    emptyState([prior]),
  );

  assert.equal(reconciled.payload.pages[1].strokes[0].sourceUuid, 'stable-uuid');
  assert.equal('nativeElementUuid' in reconciled.payload.pages[1].strokes[0], false);
  assert.equal(reconciled.ledger.entries[0].nativeUuid, 'changed-native-uuid');
});

test('a unique lasso translation preserves the stable identity', () => {
  const ledger = {
    schemaVersion: 1,
    documentId,
    entries: [{
      stableUuid: 'stable-uuid',
      nativeUuid: 'old-native-uuid',
      pageIndex: 2,
      nativeStyle: style,
      samples: original,
    }],
  };
  const moved = original.map(([x, y, pressure]) => [x + 0.15, y - 0.05, pressure]);
  const current = payload([
    {pageIndex: 1, strokes: []},
    {
      pageIndex: 2,
      strokes: [stroke('new-native-uuid', moved, {thickness: 398})],
    },
  ]);
  const reconciled = reconcileStableStrokeIdentities(
    documentId,
    current,
    emptyState([], ledger),
  );
  assert.equal(reconciled.payload.pages[1].strokes[0].sourceUuid, 'stable-uuid');
  assert.equal(reconciled.ledger.entries[0].nativeStyle.thickness, 398);
});

test('a unique cross-half translation retains identity across represented pages', () => {
  const ledger = {
    schemaVersion: 1,
    documentId,
    entries: [{
      stableUuid: 'stable-uuid',
      nativeUuid: 'old-native-uuid',
      pageIndex: 2,
      nativeStyle: style,
      samples: original,
    }],
  };
  const moved = original.map(([x, y, pressure]) => [x + 0.1, y, pressure]);
  const current = payload([
    {pageIndex: 1, strokes: [stroke('new-native-uuid', moved)]},
    {pageIndex: 2, strokes: []},
  ]);
  const reconciled = reconcileStableStrokeIdentities(
    documentId,
    current,
    emptyState([], ledger),
  );
  assert.equal(reconciled.payload.pages[0].strokes[0].sourceUuid, 'stable-uuid');
  assert.equal(reconciled.ledger.entries[0].pageIndex, 1);
});

test('ambiguous identical shapes fail closed', () => {
  const ledger = {
    schemaVersion: 1,
    documentId,
    entries: ['one', 'two'].map((stableUuid, index) => ({
      stableUuid,
      nativeUuid: `old-${index}`,
      pageIndex: 2,
      nativeStyle: style,
      samples: original.map(([x, y, pressure]) => [x + index * 0.2, y, pressure]),
    })),
  };
  const moved = original.map(([x, y, pressure]) => [x + 0.4, y, pressure]);
  assert.throws(
    () => reconcileStableStrokeIdentities(
      documentId,
      payload([{pageIndex: 2, strokes: [stroke('new-native', moved)]}]),
      emptyState([], ledger),
    ),
    /ambiguous/,
  );
});

test('represented-page deletion retires the old identity', () => {
  const ledger = {
    schemaVersion: 1,
    documentId,
    entries: [{
      stableUuid: 'deleted-stroke',
      nativeUuid: 'old-native',
      pageIndex: 2,
      nativeStyle: style,
      samples: original,
    }],
  };
  const reconciled = reconcileStableStrokeIdentities(
    documentId,
    payload([{pageIndex: 2, strokes: []}]),
    emptyState([], ledger),
  );
  assert.deepEqual(reconciled.ledger.entries, []);
});

test('retained InkBridge userData remains authoritative', () => {
  const current = stroke('canonical-uuid', original, {
    nativeElementUuid: 'host-uuid',
    userData: JSON.stringify({
      inkBridgeOrigin: 'inkbridge-sync',
      sourceUuid: 'canonical-uuid',
    }),
  });
  const reconciled = reconcileStableStrokeIdentities(
    documentId,
    payload([{pageIndex: 2, strokes: [current]}]),
    emptyState(),
  );
  assert.equal(reconciled.payload.pages[0].strokes[0].sourceUuid, 'canonical-uuid');
  assert.equal(reconciled.ledger.entries[0].nativeUuid, 'host-uuid');
});
