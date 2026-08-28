import {
  PluginCommAPI,
  PluginFileAPI,
  PointUtils,
} from 'sn-plugin-lib';
import {EMBEDDED_MANIFEST} from './generatedManifest';
import {
  descriptorMatches,
  geometryFingerprint,
  liveSnapshotMatches,
  operationSafetyPhases,
  parseUserData,
  supernotePenColor,
  strokeDescriptor,
  validateManifest,
} from './manifestCore';
import {
  requireCompatibleTargetFileName,
  requireSameDocumentPath,
} from './folderCompanionCore';
import {manifestToVirtualSpread} from './virtualSpreadAdapterCore';

async function requireResult(promise, label) {
  const response = await promise;
  if (!response?.success) {
    throw new Error(response?.error?.message ?? `${label} failed`);
  }
  return response.result;
}

function basename(path) {
  const slash = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
  return slash >= 0 ? path.slice(slash + 1) : path;
}

function samplesToEmr(samples, pageSize, normalizedYOffset) {
  const maxX = Math.max(1, pageSize.width - 1);
  const maxY = Math.max(1, pageSize.height - 1);
  return samples.map(([normalizedX, normalizedY]) => {
    const pixel = {
      x: Math.max(0, Math.min(maxX, normalizedX * maxX)),
      y: Math.max(
        0,
        Math.min(maxY, (normalizedY + normalizedYOffset) * maxY),
      ),
    };
    return PointUtils.androidPoint2Emr(pixel, pageSize);
  });
}

function samplePressures(samples) {
  return samples.map(([, , pressure]) =>
    Math.max(0, Math.min(4096, Math.round(pressure ?? 1024))),
  );
}

async function serializeNativeStroke(element, pageSize, pageIndex) {
  const pointCount = await element.stroke.points.size();
  if (!pointCount) return null;
  const emrPoints = await element.stroke.points.getRange(0, pointCount);
  const pressureCount = await element.stroke.pressures.size();
  const nativePressures =
    pressureCount > 0
      ? await element.stroke.pressures.getRange(0, pressureCount)
      : [];
  const maxX = Math.max(1, pageSize.width - 1);
  const maxY = Math.max(1, pageSize.height - 1);
  const samples = emrPoints.map((point, index) => {
    const pixel = PointUtils.emrPoint2Android(point, pageSize);
    return [
      Math.max(0, Math.min(1, pixel.x / maxX)),
      Math.max(0, Math.min(1, pixel.y / maxY)),
      Math.max(
        0,
        Math.min(4096, Math.round(nativePressures[index] ?? nativePressures[0] ?? 1024)),
      ),
    ];
  });
  const nativeStyle = {
    layerNum: element.layerNum ?? 0,
    thickness: element.thickness ?? 400,
    penColor: element.stroke.penColor ?? 0x00,
    penType: element.stroke.penType ?? 16,
  };
  return {
    sourceUuid: element.uuid ?? '',
    origin: 'supernote-native',
    pageIndex,
    nativeStyle,
    samples,
    geometryFingerprint: geometryFingerprint(nativeStyle, samples),
  };
}

async function describeElements(elements, pageSize, pageIndex) {
  const described = [];
  for (const element of elements) {
    if (element?.type !== 0 || !element?.stroke) continue;
    const snapshot = await serializeNativeStroke(element, pageSize, pageIndex);
    if (!snapshot) continue;
    described.push({
      element,
      snapshot,
      descriptor: strokeDescriptor(snapshot),
      data: parseUserData(element.userData),
    });
  }
  return described;
}

function taggedMatch(described, sourceUuid) {
  return described.find(
    item =>
      item.data?.inkBridgeOrigin === 'inkbridge-sync' &&
      item.data?.sourceUuid === sourceUuid,
  );
}

function currentAfterMatch(described, operation, yOffset) {
  const fingerprint = operation.after?.geometryFingerprint;
  return described.find(
    item =>
      (item.data?.inkBridgeOrigin === 'inkbridge-sync' &&
        item.data?.sourceUuid === operation.sourceUuid &&
        item.data?.contentHash === fingerprint &&
        liveSnapshotMatches(item.snapshot, operation.after, yOffset)) ||
      (item.snapshot.geometryFingerprint === fingerprint &&
        (item.element.uuid === operation.sourceUuid ||
          item.data?.sourceUuid === operation.sourceUuid)),
  );
}

function exactIdentityMatch(described, sourceUuid) {
  return described.find(
    item =>
      item.element.uuid === sourceUuid || item.data?.sourceUuid === sourceUuid,
  );
}

function sameNativeElement(left, right) {
  if (!left || !right) return false;
  if (left.element.uuid && right.element.uuid) {
    return left.element.uuid === right.element.uuid;
  }
  return (
    Number.isInteger(left.element.numInPage) &&
    left.element.numInPage === right.element.numInPage
  );
}

function uniqueGeometryMatch(described, snapshot) {
  if (!snapshot) return null;
  const expected = strokeDescriptor(snapshot);
  const matches = described.filter(item =>
    descriptorMatches(item.descriptor, expected),
  );
  return matches.length === 1 ? matches[0] : null;
}

function findTarget(described, operation) {
  return (
    taggedMatch(described, operation.sourceUuid) ??
    exactIdentityMatch(described, operation.sourceUuid) ??
    uniqueGeometryMatch(described, operation.before)
  );
}

function findSupersededTarget(described, operation, current, previousTarget) {
  const candidates = current
    ? described.filter(item => !sameNativeElement(item, current))
    : described;
  const exact = exactIdentityMatch(candidates, operation.sourceUuid);
  if (exact) return exact;
  const geometry = uniqueGeometryMatch(
    candidates,
    operation.before ?? previousTarget?.snapshot,
  );
  if (geometry) return geometry;
  if (Number.isInteger(previousTarget?.element?.numInPage)) {
    return candidates.find(
      item => item.element.numInPage === previousTarget.element.numInPage,
    );
  }
  return null;
}

async function prepareNativeStroke({
  pageSize,
  snapshot,
  yOffset,
  manifestId,
}) {
  const target = await requireResult(
    PluginCommAPI.createElement(0),
    'createElement',
  );
  if (!target?.stroke) {
    throw new Error('createElement returned an element without stroke accessors.');
  }
  const {nativeStyle} = snapshot;
  target.layerNum = nativeStyle.layerNum ?? 0;
  target.thickness = nativeStyle.thickness;
  target.stroke.penColor = supernotePenColor(nativeStyle.penColor);
  target.stroke.penType = nativeStyle.penType;
  target.userData = JSON.stringify({
    inkBridgeOrigin: 'inkbridge-sync',
    sourceUuid: snapshot.sourceUuid,
    sourcePenColor: nativeStyle.penColor,
    contentHash: snapshot.geometryFingerprint,
    manifestId,
  });

  const points = samplesToEmr(snapshot.samples, pageSize, yOffset);
  const pressures = samplePressures(snapshot.samples);
  const pointsOk = await target.stroke.points.setRange(
    0,
    points.length - 1,
    points,
  );
  if (!pointsOk) throw new Error('Could not write native stroke points.');
  const pressuresOk = await target.stroke.pressures.setRange(
    0,
    pressures.length - 1,
    pressures,
  );
  if (!pressuresOk) throw new Error('Could not write native stroke pressures.');
  return target;
}

async function insertTargets(filePath, pageIndex, targets) {
  if (!targets.length) return;
  await requireResult(
    PluginFileAPI.insertElements(filePath, pageIndex, targets),
    'insertElements',
  );
}

async function deleteTargets(filePath, pageIndex, targets) {
  if (!targets.length) return;
  const indices = [];
  for (const {target, label} of targets) {
    if (!Number.isInteger(target?.element?.numInPage)) {
      throw new Error(`Could not resolve native element index for ${label}.`);
    }
    if (!indices.includes(target.element.numInPage)) {
      indices.push(target.element.numInPage);
    }
  }
  // Descending order is safe whether the host treats the indices as a set or
  // removes them one by one while compacting the page element list.
  indices.sort((left, right) => right - left);
  await requireResult(
    PluginFileAPI.deleteElements(filePath, pageIndex, indices),
    'deleteElements',
  );
}

function operationsByPage(indexedOperations) {
  const pages = new Map();
  indexedOperations.forEach(({operation, index}) => {
    const page = pages.get(operation.pageIndex) ?? [];
    page.push({operation, index});
    pages.set(operation.pageIndex, page);
  });
  return pages;
}

async function applyPage({
  filePath,
  pageIndex,
  indexedOperations,
  operationCount,
  yOffset,
  manifestId,
  counts,
}) {
  const pageSize = await requireResult(
    PluginFileAPI.getPageSize(filePath, pageIndex),
    `getPageSize page ${pageIndex + 1}`,
  );
  const elements =
    (await requireResult(
      PluginFileAPI.getElements(pageIndex, filePath),
      `getElements page ${pageIndex + 1}`,
    )) ?? [];
  const described = await describeElements(elements, pageSize, pageIndex);
  const insertions = [];
  const deletions = [];
  const outcomes = [];

  console.log(
    `INKBRIDGE_SYNC_PAGE page=${pageIndex + 1} operations=${indexedOperations.length} nativeStrokes=${described.length} stage=scanned`,
  );

  for (const {operation, index} of indexedOperations) {
    const target = findTarget(described, operation);
    if (operation.type === 'delete_stroke') {
      if (!target) {
        outcomes.push({index, operation, result: 'already-absent'});
      } else {
        deletions.push({target, label: operation.sourceUuid});
        outcomes.push({index, operation, result: 'applied'});
      }
      continue;
    }

    const current = currentAfterMatch(described, operation, yOffset);
    if (current) {
      const superseded = operation.before
        ? findSupersededTarget(described, operation, current, null)
        : null;
      if (superseded) {
        deletions.push({
          target: superseded,
          label: `superseded ${operation.sourceUuid}`,
        });
        outcomes.push({index, operation, result: 'repaired'});
      } else {
        outcomes.push({index, operation, result: 'already-current'});
      }
      continue;
    }

    insertions.push(
      await prepareNativeStroke({
        pageSize,
        snapshot: operation.after,
        yOffset,
        manifestId,
      }),
    );
    if (target) {
      deletions.push({
        target,
        label: `superseded ${operation.sourceUuid}`,
      });
      outcomes.push({index, operation, result: 'updated'});
    } else {
      outcomes.push({index, operation, result: 'added'});
    }
  }

  console.log(
    `INKBRIDGE_SYNC_PAGE page=${pageIndex + 1} insert=${insertions.length} delete=${deletions.length} stage=planned`,
  );
  await insertTargets(filePath, pageIndex, insertions);
  if (insertions.length) {
    console.log(
      `INKBRIDGE_SYNC_PAGE page=${pageIndex + 1} inserted=${insertions.length} stage=inserted`,
    );
  }
  await deleteTargets(filePath, pageIndex, deletions);
  if (deletions.length) {
    console.log(
      `INKBRIDGE_SYNC_PAGE page=${pageIndex + 1} deleted=${deletions.length} stage=deleted`,
    );
  }

  for (const {index, operation, result} of outcomes) {
    if (operation.type === 'delete_stroke') {
      if (result === 'applied') counts.deleted += 1;
      else counts.skipped += 1;
      console.log(
        `INKBRIDGE_SYNC_OP ${index + 1}/${operationCount} delete ${operation.sourceUuid} ${result}`,
      );
      continue;
    }
    if (result === 'added') counts.added += 1;
    else if (result === 'updated' || result === 'repaired') counts.updated += 1;
    else counts.skipped += 1;
    console.log(
      `INKBRIDGE_SYNC_OP ${index + 1}/${operationCount} upsert ${operation.sourceUuid} ${result}`,
    );
  }
}

export async function applyManifest(
  inputManifest,
  expectedFilePath = null,
  stableIdentityValidated = false,
) {
  const manifest = validateManifest(inputManifest);
  const filePath = await requireResult(
    PluginCommAPI.getCurrentFilePath(),
    'getCurrentFilePath',
  );
  requireSameDocumentPath(expectedFilePath, filePath);
  const targetNames = manifest.document?.targetFileNames ?? [];
  const openName = basename(filePath);
  requireCompatibleTargetFileName(
    targetNames,
    openName,
    stableIdentityValidated,
  );
  const yOffset =
    manifest.coordinateTransform?.pdfToSupernoteNormalizedYOffset ?? 0;
  const counts = {added: 0, updated: 0, deleted: 0, skipped: 0};

  // Complete every destination insertion before processing explicit source
  // deletions. If a cross-page move is interrupted, this ordering can leave a
  // repairable duplicate but never removes the only native copy.
  for (const phase of operationSafetyPhases(manifest.operations)) {
    for (const [pageIndex, indexedOperations] of operationsByPage(phase)) {
      await applyPage({
        filePath,
        pageIndex,
        indexedOperations,
        operationCount: manifest.operations.length,
        yOffset,
        manifestId: manifest.manifestId,
        counts,
      });
    }
  }

  const currentBeforeReload = await requireResult(
    PluginCommAPI.getCurrentFilePath(),
    'getCurrentFilePath before reload',
  );
  requireSameDocumentPath(filePath, currentBeforeReload);
  await requireResult(PluginCommAPI.reloadFile(), 'reloadFile');
  return {
    manifestId: manifest.manifestId,
    operationCount: manifest.operations.length,
    ...counts,
  };
}

export async function applyEmbeddedManifest() {
  return applyManifest(EMBEDDED_MANIFEST);
}

export async function applyVirtualSpreadManifest(
  inputManifest,
  representation,
  expectedFilePath = null,
  nativeViewports = null,
) {
  const canonical = validateManifest(inputManifest);
  const filePath = expectedFilePath ?? await requireResult(
    PluginCommAPI.getCurrentFilePath(),
    'getCurrentFilePath before Virtual Spread transform',
  );
  const targetPages = new Set();
  for (const operation of canonical.operations) {
    const mapping = representation.mappings.find(
      candidate => candidate.sourcePageIndex === operation.pageIndex,
    );
    if (!mapping) {
      throw new Error(
        `No Virtual Spread mapping exists for source page ${operation.pageIndex + 1}.`,
      );
    }
    targetPages.add(mapping.virtualPageIndex);
  }
  const nativePageSizes = new Map();
  for (const pageIndex of targetPages) {
    nativePageSizes.set(
      pageIndex,
      await requireResult(
        PluginFileAPI.getPageSize(filePath, pageIndex),
        `getPageSize page ${pageIndex + 1} before Virtual Spread transform`,
      ),
    );
  }
  return applyManifest(
    manifestToVirtualSpread(
      canonical,
      representation,
      nativeViewports,
      nativePageSizes,
    ),
    filePath,
    true,
  );
}
