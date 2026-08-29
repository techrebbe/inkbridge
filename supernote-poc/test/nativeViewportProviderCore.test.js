import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import {
  completedVirtualSpreadDelivery,
  finishVirtualSpreadStep,
  nativeViewportMap,
  planVirtualSpreadDelivery,
  requireNativeViewportResult,
  requireSameNativeViewport,
} from '../overlay/nativeViewportProviderCore.js';
import {PAGE_143_VIRTUAL_SPREAD_FIXTURE} from '../overlay/virtualSpreadFixture.js';

const descriptorFixture = readFileSync(
  new URL('./fixtures/page-143-native-viewport-v1.json', import.meta.url),
);
const descriptorJson = descriptorFixture.toString('utf8').trimEnd();
const descriptor = Object.freeze(JSON.parse(descriptorJson));

test('imports the frozen RTL Reader descriptor bytes and both normative hashes', () => {
  assert.equal(
    createHash('sha256').update(descriptorFixture).digest('hex'),
    '27145685a793ce2716a5da6c26db4a1fa64bac0e1ad6bc1329e0c502326a48e4',
  );
  assert.equal(
    createHash('sha256').update(descriptorJson).digest('hex'),
    'a590afc7a95e92fbf7b9ac03fd949bcd6b474bcba70e06e4ec63936de937d033',
  );
});

function result(overrides = {}) {
  return {
    protocolVersion: 1,
    status: 'ok',
    descriptor,
    descriptorSha256:
      'a590afc7a95e92fbf7b9ac03fd949bcd6b474bcba70e06e4ec63936de937d033',
    snapshotId: 'pdf-identity:sidecar-identity',
    verificationGeneration: 4,
    pageLoadGeneration: 9,
    publishedAtElapsedRealtime: 123456,
    ...overrides,
  };
}

test('accepts the normative page-143 viewport and maps only its live page', () => {
  const accepted = requireNativeViewportResult(
    result(),
    PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    1,
    {width: 1872, height: 1404},
  );
  const viewports = nativeViewportMap(accepted);
  assert.equal(viewports.size, 1);
  assert.equal(viewports.get(1), descriptor);
});

test('fails closed while RTL Reader has no fresh matching viewport', () => {
  assert.throws(
    () => requireNativeViewportResult(
      {protocolVersion: 1, status: 'unavailable'},
      PAGE_143_VIRTUAL_SPREAD_FIXTURE,
      1,
      {width: 1872, height: 1404},
    ),
    /still loading or no longer matches/,
  );
});

test('rejects extra response fields and mismatched descriptor authority', () => {
  assert.throws(
    () => requireNativeViewportResult(
      result({unexpected: true}),
      PAGE_143_VIRTUAL_SPREAD_FIXTURE,
      1,
      {width: 1872, height: 1404},
    ),
    /unknown fields/,
  );
  assert.throws(
    () => requireNativeViewportResult(
      result({descriptor: {...descriptor, virtualPageIndex: 0}}),
      PAGE_143_VIRTUAL_SPREAD_FIXTURE,
      1,
      {width: 1872, height: 1404},
    ),
    /authoritative RTL Reader native viewport is required/,
  );
});

test('rejects a page-load or activated-snapshot change during collection', () => {
  assert.throws(
    () => requireSameNativeViewport(
      result(),
      result({pageLoadGeneration: 10}),
    ),
    /changed while InkBridge was collecting/,
  );
  assert.throws(
    () => requireSameNativeViewport(
      result(),
      result({snapshotId: 'replacement:sidecar'}),
    ),
    /changed while InkBridge was collecting/,
  );
  assert.equal(requireSameNativeViewport(result(), result()).status, 'ok');
});

test('an apply-time viewport change cannot commit progress or trigger a redraw', async () => {
  const calls = [];
  await assert.rejects(
    finishVirtualSpreadStep({
      expectedViewport: result(),
      readCurrentViewport: async () => result({pageLoadGeneration: 10}),
      recordProgress: async () => calls.push('record'),
      reload: async () => calls.push('reload'),
    }),
    /changed while InkBridge was collecting/,
  );
  assert.deepEqual(calls, []);
});

test('the post-apply fence and progress commit precede the plugin redraw', async () => {
  const calls = [];
  const progress = await finishVirtualSpreadStep({
    expectedViewport: result(),
    readCurrentViewport: async () => {
      calls.push('fence');
      return result();
    },
    recordProgress: async () => {
      calls.push('record');
      return {completed: true};
    },
    reload: async () => calls.push('reload'),
  });
  assert.deepEqual(calls, ['fence', 'record', 'reload']);
  assert.deepEqual(progress, {completed: true});
});

test('rejects unsafe generation and publication evidence', () => {
  for (const overrides of [
    {verificationGeneration: -1},
    {pageLoadGeneration: Number.MAX_SAFE_INTEGER + 1},
    {publishedAtElapsedRealtime: 1.5},
  ]) {
    assert.throws(
      () => requireNativeViewportResult(
        result(overrides),
        PAGE_143_VIRTUAL_SPREAD_FIXTURE,
        1,
        {width: 1872, height: 1404},
      ),
      /invalid/,
    );
  }
});

const emptyProgress = Object.freeze({
  completedStepIds: [],
  summary: {
    operationCount: 0,
    added: 0,
    updated: 0,
    deleted: 0,
    skipped: 0,
  },
});

const multiSpreadManifest = Object.freeze({
  schemaVersion: 1,
  manifestId: 'multi-spread',
  operations: [
    {type: 'upsert_stroke', sourceUuid: 'cover', pageIndex: 0},
    {type: 'upsert_stroke', sourceUuid: 'right', pageIndex: 1},
    {type: 'delete_stroke', sourceUuid: 'left', pageIndex: 2},
  ],
});

test('stages a multi-spread manifest against only the currently authorized page', () => {
  const first = planVirtualSpreadDelivery(
    multiSpreadManifest,
    PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    1,
    emptyProgress,
  );
  assert.deepEqual(first.steps.map(step => step.id), ['upsert:0', 'upsert:1', 'delete:1']);
  assert.deepEqual(
    first.manifest.operations.map(operation => operation.sourceUuid),
    ['right'],
  );

  const afterFirst = {
    completedStepIds: ['upsert:1'],
    summary: {
      operationCount: 1,
      added: 1,
      updated: 0,
      deleted: 0,
      skipped: 0,
    },
  };
  const waiting = planVirtualSpreadDelivery(
    multiSpreadManifest,
    PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    1,
    afterFirst,
  );
  assert.equal(waiting.manifest, null);
  assert.equal(waiting.nextPage, 0);

  const second = planVirtualSpreadDelivery(
    multiSpreadManifest,
    PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    0,
    afterFirst,
  );
  assert.deepEqual(
    second.manifest.operations.map(operation => operation.sourceUuid),
    ['cover'],
  );
});

test('completes and aggregates a page-staged manifest only after every operation', () => {
  const progress = {
    completedStepIds: ['delete:1', 'upsert:0', 'upsert:1'],
    summary: {
      operationCount: 3,
      added: 2,
      updated: 0,
      deleted: 1,
      skipped: 0,
    },
  };
  const plan = planVirtualSpreadDelivery(
    multiSpreadManifest,
    PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    1,
    progress,
  );
  assert.equal(plan.complete, true);
  assert.deepEqual(
    completedVirtualSpreadDelivery(
      multiSpreadManifest,
      plan.steps,
      progress,
    ),
    {
      complete: true,
      manifestId: 'multi-spread',
      operationCount: 3,
      added: 2,
      updated: 0,
      deleted: 1,
      skipped: 0,
    },
  );
  assert.throws(
    () => planVirtualSpreadDelivery(
      multiSpreadManifest,
      PAGE_143_VIRTUAL_SPREAD_FIXTURE,
      1,
      {...progress, summary: {...progress.summary, operationCount: 2}},
    ),
    /does not cover every operation/,
  );
});

test('cross-spread move inserts the destination before allowing source deletion', () => {
  const manifest = {
    manifestId: 'move',
    operations: [
      {type: 'delete_stroke', sourceUuid: 'move-me', pageIndex: 0},
      {type: 'upsert_stroke', sourceUuid: 'move-me', pageIndex: 2},
    ],
  };
  const sourceFirst = planVirtualSpreadDelivery(
    manifest,
    PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    0,
    emptyProgress,
  );
  assert.equal(sourceFirst.manifest, null);
  assert.equal(sourceFirst.nextPage, 1);
  const afterDestination = {
    completedStepIds: ['upsert:1'],
    summary: {operationCount: 1, added: 1, updated: 0, deleted: 0, skipped: 0},
  };
  const sourceAfter = planVirtualSpreadDelivery(
    manifest,
    PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    0,
    afterDestination,
  );
  assert.equal(sourceAfter.stepId, 'delete:0');
  assert.equal(sourceAfter.manifest.operations[0].type, 'delete_stroke');
});

test('same-spread cross-half move remains one indivisible transformation step', () => {
  const manifest = {
    manifestId: 'cross-half',
    operations: [
      {type: 'delete_stroke', sourceUuid: 'move-me', pageIndex: 1},
      {type: 'upsert_stroke', sourceUuid: 'move-me', pageIndex: 2},
    ],
  };
  const plan = planVirtualSpreadDelivery(
    manifest,
    PAGE_143_VIRTUAL_SPREAD_FIXTURE,
    1,
    emptyProgress,
  );
  assert.deepEqual(plan.steps.map(step => step.id), ['upsert:1']);
  assert.equal(plan.manifest.operations.length, 2);
});
