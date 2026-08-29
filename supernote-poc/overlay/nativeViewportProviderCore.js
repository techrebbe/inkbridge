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
const PROGRESS_KEYS = new Set([
  'completedStepIds',
  'summary',
]);
const SUMMARY_KEYS = new Set([
  'operationCount',
  'added',
  'updated',
  'deleted',
  'skipped',
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

export async function finishVirtualSpreadStep({
  expectedViewport,
  readCurrentViewport,
  recordProgress,
  reload,
}) {
  requireSameNativeViewport(expectedViewport, await readCurrentViewport());
  const progress = await recordProgress();
  await reload();
  return progress;
}

function requireSummary(value) {
  if (
    !value ||
    typeof value !== 'object' ||
    Object.keys(value).length !== SUMMARY_KEYS.size ||
    Object.keys(value).some(key => !SUMMARY_KEYS.has(key))
  ) {
    throw new Error('Virtual Spread delivery progress has an invalid summary.');
  }
  for (const key of SUMMARY_KEYS) {
    requireNonnegativeSafeInteger(
      value[key],
      `Virtual Spread delivery progress ${key}`,
    );
  }
  return value;
}

export function requireVirtualSpreadProgress(progress) {
  if (
    !progress ||
    typeof progress !== 'object' ||
    Object.keys(progress).length !== PROGRESS_KEYS.size ||
    Object.keys(progress).some(key => !PROGRESS_KEYS.has(key)) ||
    !Array.isArray(progress.completedStepIds)
  ) {
    throw new Error('Virtual Spread delivery progress is invalid.');
  }
  const completed = progress.completedStepIds;
  completed.forEach((stepId, index) => {
    if (
      typeof stepId !== 'string' ||
      !/^(upsert|delete):(0|[1-9][0-9]*)$/.test(stepId) ||
      Number(stepId.split(':')[1]) > 2147483647
    ) {
      throw new Error('Virtual Spread completed step identity is invalid.');
    }
    if (index && completed[index - 1] >= stepId) {
      throw new Error(
        'Virtual Spread completed step identities are duplicated or unordered.',
      );
    }
  });
  requireSummary(progress.summary);
  return progress;
}

function virtualPageForOperation(operation, representation) {
  const mapping = representation?.mappings?.find(
    candidate => candidate.sourcePageIndex === operation?.pageIndex,
  );
  if (!mapping) {
    throw new Error(
      `No Virtual Spread mapping exists for source page ${Number(operation?.pageIndex) + 1}.`,
    );
  }
  return mapping.virtualPageIndex;
}

function virtualSpreadSteps(manifest, representation) {
  const upserts = new Map();
  const upsertPages = new Map();
  const deletePages = new Map();
  function append(pages, page, operation) {
    const operations = pages.get(page) ?? [];
    operations.push(operation);
    pages.set(page, operations);
  }
  for (const operation of manifest.operations) {
    if (!['upsert_stroke', 'delete_stroke'].includes(operation?.type)) {
      throw new Error('Virtual Spread manifest contains an unsupported operation.');
    }
    if (operation.type !== 'upsert_stroke') continue;
    if (!operation.sourceUuid || upserts.has(operation.sourceUuid)) {
      throw new Error('Virtual Spread manifest contains duplicate or missing stroke identities.');
    }
    const page = virtualPageForOperation(operation, representation);
    upserts.set(operation.sourceUuid, page);
    append(upsertPages, page, operation);
  }
  for (const operation of manifest.operations) {
    if (operation.type !== 'delete_stroke') continue;
    const page = virtualPageForOperation(operation, representation);
    // A move between source halves on one physical spread is transformed as
    // one upsert with its old geometry. Keep that pair in the same step.
    const pages = upserts.get(operation.sourceUuid) === page
      ? upsertPages
      : deletePages;
    append(pages, page, operation);
  }
  return [
    ...[...upsertPages].sort(([left], [right]) => left - right).map(
      ([page, operations]) => ({id: `upsert:${page}`, page, phase: 'upsert', operations}),
    ),
    ...[...deletePages].sort(([left], [right]) => left - right).map(
      ([page, operations]) => ({id: `delete:${page}`, page, phase: 'delete', operations}),
    ),
  ];
}

function validateProgressCoverage(steps, inputProgress) {
  const progress = requireVirtualSpreadProgress(inputProgress);
  const completed = progress.completedStepIds;
  if (completed.some(id => !steps.some(step => step.id === id))) {
    throw new Error('Virtual Spread delivery progress names a step outside this manifest.');
  }
  const expectedCount = steps
    .filter(step => completed.includes(step.id))
    .reduce((count, step) => count + step.operations.length, 0);
  if (progress.summary.operationCount !== expectedCount) {
    throw new Error('Virtual Spread delivery progress does not cover every operation.');
  }
  const remainingUpserts = steps.some(
    step => step.phase === 'upsert' && !completed.includes(step.id),
  );
  if (remainingUpserts && completed.some(id => id.startsWith('delete:'))) {
    throw new Error('Virtual Spread source deletions precede destination insertion.');
  }
  return progress;
}

export function planVirtualSpreadDelivery(
  manifest,
  representation,
  currentVirtualPageIndex,
  inputProgress,
) {
  if (!manifest || !Array.isArray(manifest.operations)) {
    throw new Error('InkBridge manifest has no operations to stage.');
  }
  requireNonnegativeSafeInteger(
    currentVirtualPageIndex,
    'Current Virtual Spread page index',
  );
  const steps = virtualSpreadSteps(manifest, representation);
  const progress = validateProgressCoverage(steps, inputProgress);
  const remaining = steps.filter(
    step => !progress.completedStepIds.includes(step.id),
  );
  if (!remaining.length) return {complete: true, steps, progress};
  const phase = remaining[0].phase;
  const current = remaining.find(
    step => step.phase === phase && step.page === currentVirtualPageIndex,
  );
  if (!current) {
    return {
      complete: false,
      steps,
      progress,
      nextPage: remaining[0].page,
      manifest: null,
    };
  }
  return {
    complete: false,
    steps,
    progress,
    nextPage: currentVirtualPageIndex,
    stepId: current.id,
    manifest: {...manifest, operations: current.operations},
  };
}

export function completedVirtualSpreadDelivery(
  manifest,
  steps,
  inputProgress,
) {
  const progress = validateProgressCoverage(steps, inputProgress);
  const complete = steps.every(step =>
    progress.completedStepIds.includes(step.id),
  );
  if (complete && progress.summary.operationCount !== manifest.operations.length) {
    throw new Error(
      'Virtual Spread delivery progress does not cover every operation.',
    );
  }
  return {
    complete,
    ...progress.summary,
    manifestId: manifest.manifestId,
  };
}
