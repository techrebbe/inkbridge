import {
  PluginCommAPI,
  PluginFileAPI,
  PointUtils,
} from 'sn-plugin-lib';
import {EMBEDDED_MANIFEST} from './generatedManifest';
import {
  descriptorMatches,
  geometryFingerprint,
  parseUserData,
  strokeDescriptor,
  validateManifest,
} from './manifestCore';

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

function exactIdentityMatch(described, sourceUuid) {
  return described.find(
    item =>
      item.element.uuid === sourceUuid || item.data?.sourceUuid === sourceUuid,
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

async function createNativeStroke({
  filePath,
  pageIndex,
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
  target.stroke.penColor = nativeStyle.penColor;
  target.stroke.penType = nativeStyle.penType;
  target.userData = JSON.stringify({
    inkBridgeOrigin: 'inkbridge-sync',
    sourceUuid: snapshot.sourceUuid,
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
  await requireResult(
    PluginFileAPI.insertElements(filePath, pageIndex, [target]),
    'insertElements',
  );
}

async function deleteTarget(filePath, pageIndex, target, label) {
  if (!Number.isInteger(target?.element?.numInPage)) {
    throw new Error(`Could not resolve native element index for ${label}.`);
  }
  await requireResult(
    PluginFileAPI.deleteElements(filePath, pageIndex, [
      target.element.numInPage,
    ]),
    `deleteElements ${label}`,
  );
}

export async function applyEmbeddedManifest() {
  const manifest = validateManifest(EMBEDDED_MANIFEST);
  const filePath = await requireResult(
    PluginCommAPI.getCurrentFilePath(),
    'getCurrentFilePath',
  );
  const targetNames = manifest.document?.targetFileNames ?? [];
  const openName = basename(filePath);
  if (targetNames.length && !targetNames.includes(openName)) {
    throw new Error(
      `This sync package targets ${targetNames.join(', ')}, but ${openName} is open.`,
    );
  }
  const yOffset =
    manifest.coordinateTransform?.pdfToSupernoteNormalizedYOffset ?? 0;
  const counts = {added: 0, updated: 0, deleted: 0, skipped: 0};

  for (let index = 0; index < manifest.operations.length; index += 1) {
    const operation = manifest.operations[index];
    const pageSize = await requireResult(
      PluginFileAPI.getPageSize(filePath, operation.pageIndex),
      `getPageSize page ${operation.pageIndex + 1}`,
    );
    let elements =
      (await requireResult(
        PluginFileAPI.getElements(operation.pageIndex, filePath),
        `getElements page ${operation.pageIndex + 1}`,
      )) ?? [];
    let described = await describeElements(
      elements,
      pageSize,
      operation.pageIndex,
    );
    const target = findTarget(described, operation);

    if (operation.type === 'delete_stroke') {
      if (!target) {
        counts.skipped += 1;
        console.log(
          `INKBRIDGE_SYNC_OP ${index + 1}/${manifest.operations.length} delete ${operation.sourceUuid} already-absent`,
        );
        continue;
      }
      await deleteTarget(
        filePath,
        operation.pageIndex,
        target,
        operation.sourceUuid,
      );
      counts.deleted += 1;
      console.log(
        `INKBRIDGE_SYNC_OP ${index + 1}/${manifest.operations.length} delete ${operation.sourceUuid} applied`,
      );
      continue;
    }

    const after = operation.after;
    const tagged = taggedMatch(described, operation.sourceUuid);
    const exactAfter =
      tagged?.data?.contentHash === after.geometryFingerprint ||
      described.some(
        item =>
          item.snapshot.geometryFingerprint === after.geometryFingerprint &&
          (item.element.uuid === operation.sourceUuid ||
            item.data?.sourceUuid === operation.sourceUuid),
      );
    if (exactAfter) {
      counts.skipped += 1;
      console.log(
        `INKBRIDGE_SYNC_OP ${index + 1}/${manifest.operations.length} upsert ${operation.sourceUuid} already-current`,
      );
      continue;
    }

    await createNativeStroke({
      filePath,
      pageIndex: operation.pageIndex,
      pageSize,
      snapshot: after,
      yOffset,
      manifestId: manifest.manifestId,
    });

    if (target) {
      // Re-fetch after insertion: Supernote can renumber native elements.
      elements =
        (await requireResult(
          PluginFileAPI.getElements(operation.pageIndex, filePath),
          'getElements after replacement insert',
        )) ?? [];
      described = await describeElements(
        elements,
        pageSize,
        operation.pageIndex,
      );
      const superseded =
        described.find(item => item.element.uuid === target.element.uuid) ??
        uniqueGeometryMatch(described, target.snapshot);
      if (!superseded) {
        throw new Error(
          `Inserted ${operation.sourceUuid}, but could not locate its superseded native stroke.`,
        );
      }
      await deleteTarget(
        filePath,
        operation.pageIndex,
        superseded,
        `superseded ${operation.sourceUuid}`,
      );
      counts.updated += 1;
      console.log(
        `INKBRIDGE_SYNC_OP ${index + 1}/${manifest.operations.length} upsert ${operation.sourceUuid} updated`,
      );
    } else {
      counts.added += 1;
      console.log(
        `INKBRIDGE_SYNC_OP ${index + 1}/${manifest.operations.length} upsert ${operation.sourceUuid} added`,
      );
    }
  }

  await requireResult(PluginCommAPI.reloadFile(), 'reloadFile');
  return {
    manifestId: manifest.manifestId,
    operationCount: manifest.operations.length,
    ...counts,
  };
}
