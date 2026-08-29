import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import {
  nativeViewportMap,
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
