import {nativeViewportForVirtualSpread} from './virtualSpreadAdapterCore.js';

const SHA256 = /^[0-9a-f]{64}$/;
const RESULT_KEYS = new Set([
  'protocolVersion',
  'status',
  'descriptor',
  'descriptorSha256',
  'snapshotId',
  'verificationGeneration',
  'pageLoadGeneration',
  'publishedAtElapsedRealtime',
]);

function requireNonnegativeSafeInteger(value, label) {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${label} is invalid.`);
  }
  return value;
}

export function requireNativeViewportResult(
  result,
  representation,
  virtualPageIndex,
  nativePageSize,
) {
  if (!result || result.protocolVersion !== 1) {
    throw new Error('RTL Reader returned an unsupported viewport protocol.');
  }
  if (result.status === 'unavailable') {
    throw new Error(
      'The current Virtual Spread page is still loading or no longer matches its authenticated cache. Wait for the page to finish, then retry.',
    );
  }
  if (result.status !== 'ok') {
    throw new Error('RTL Reader returned an invalid viewport status.');
  }
  if (Object.keys(result).some(key => !RESULT_KEYS.has(key))) {
    throw new Error('The native viewport response contains unknown fields.');
  }
  for (const required of RESULT_KEYS) {
    if (!Object.hasOwn(result, required)) {
      throw new Error(`The native viewport response is missing ${required}.`);
    }
  }
  if (!SHA256.test(result.descriptorSha256)) {
    throw new Error('The native viewport descriptor hash is invalid.');
  }
  if (typeof result.snapshotId !== 'string' || !result.snapshotId) {
    throw new Error('The native viewport snapshot identity is invalid.');
  }
  requireNonnegativeSafeInteger(
    result.verificationGeneration,
    'The native viewport verification generation',
  );
  requireNonnegativeSafeInteger(
    result.pageLoadGeneration,
    'The native viewport page-load generation',
  );
  requireNonnegativeSafeInteger(
    result.publishedAtElapsedRealtime,
    'The native viewport publication time',
  );
  nativeViewportForVirtualSpread(
    representation,
    result.descriptor,
    virtualPageIndex,
    nativePageSize,
  );
  return result;
}

export function requireSameNativeViewport(expected, current) {
  if (
    !expected ||
    !current ||
    expected.descriptorSha256 !== current.descriptorSha256 ||
    expected.snapshotId !== current.snapshotId ||
    expected.verificationGeneration !== current.verificationGeneration ||
    expected.pageLoadGeneration !== current.pageLoadGeneration
  ) {
    throw new Error(
      'The active Virtual Spread page changed while InkBridge was collecting native ink. Retry the action on the intended page.',
    );
  }
  return current;
}

export function nativeViewportMap(result) {
  return new Map([
    [result.descriptor.virtualPageIndex, result.descriptor],
  ]);
}
