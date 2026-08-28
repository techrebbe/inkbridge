import React from 'react';
import {StyleSheet, Text, View} from 'react-native';
import {
  PluginCommAPI,
  PluginFileAPI,
  PointUtils,
} from 'sn-plugin-lib';
import {BOOX_NATIVE_STROKE_FIXTURE} from './booxFixture';
import {BOOX_RETURN_FIXTURE} from './booxReturnFixture';
import {requireSameDocumentPath} from './folderCompanionCore';
import {exportedStrokeIdentity} from './manifestCore';
import {
  buildVirtualSpreadSnapshot,
  nativeViewportForVirtualSpread,
} from './virtualSpreadAdapterCore';

const OFFSET_X_PX = 80;
const OFFSET_Y_PX = 50;
const LOG_CHUNK_SIZE = 1800;

async function requireResult(promise, label) {
  const response = await promise;
  if (!response?.success) {
    throw new Error(response?.error?.message ?? `${label} failed`);
  }
  return response.result;
}

async function currentDocumentContext() {
  const filePath = await requireResult(
    PluginCommAPI.getCurrentFilePath(),
    'getCurrentFilePath',
  );
  const page = await requireResult(
    PluginCommAPI.getCurrentPageNum(),
    'getCurrentPageNum',
  );
  const pageSize = await requireResult(
    PluginFileAPI.getPageSize(filePath, page),
    'getPageSize',
  );
  return {filePath, page, pageSize};
}

function samplesToEmr(samples, pageSize, normalizedYOffset = 0) {
  return samples.map(([normalizedX, normalizedY]) => {
    const correctedY = Math.max(
      0,
      Math.min(1, normalizedY + normalizedYOffset),
    );
    const pixel = {
      x: Math.max(
        0,
        Math.min(pageSize.width - 1, normalizedX * (pageSize.width - 1)),
      ),
      y: Math.max(
        0,
        Math.min(pageSize.height - 1, correctedY * (pageSize.height - 1)),
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

function parseUserData(userData) {
  if (!userData || typeof userData !== 'string') return null;
  try {
    return JSON.parse(userData);
  } catch {
    return null;
  }
}

async function createNativeStroke({
  filePath,
  page,
  points,
  pressures,
  thickness = 2,
  layerNum = 0,
  penColor = 0x00,
  penType = 16,
  userData,
}) {
  if (!points.length || points.length !== pressures.length) {
    throw new Error('Stroke point/pressure arrays must be non-empty and the same length.');
  }

  const target = await requireResult(
    PluginCommAPI.createElement(0),
    'createElement',
  );
  if (!target?.stroke) {
    throw new Error('createElement returned a stroke without stroke accessors.');
  }

  target.layerNum = layerNum;
  target.thickness = thickness;
  target.stroke.penColor = penColor;
  target.stroke.penType = penType;
  if (userData) target.userData = userData;

  const pointsOk = await target.stroke.points.setRange(
    0,
    points.length - 1,
    points,
  );
  if (!pointsOk) throw new Error('Could not write native stroke points.');

  const pressureOk = await target.stroke.pressures.setRange(
    0,
    pressures.length - 1,
    pressures,
  );
  if (!pressureOk) throw new Error('Could not write native stroke pressure data.');

  await requireResult(
    PluginFileAPI.insertElements(filePath, page, [target]),
    'insertElements',
  );

  return target;
}

export async function duplicateFirstStroke() {
  const {filePath, page, pageSize} = await currentDocumentContext();
  const elements = await requireResult(
    PluginFileAPI.getElements(page, filePath),
    'getElements',
  );

  const source = (elements ?? []).find(element => element?.type === 0 && element?.stroke);
  if (!source?.stroke) {
    throw new Error('No handwritten stroke found on the current page. Write one first, then run InkBridge Test again.');
  }

  const pointCount = await source.stroke.points.size();
  if (!pointCount) {
    throw new Error('The selected source stroke has no points.');
  }
  const sourcePoints = await source.stroke.points.getRange(0, pointCount);
  const movedPoints = sourcePoints.map(point => {
    const pixel = PointUtils.emrPoint2Android(point, pageSize);
    const moved = {
      x: Math.max(0, Math.min(pageSize.width - 1, pixel.x + OFFSET_X_PX)),
      y: Math.max(0, Math.min(pageSize.height - 1, pixel.y + OFFSET_Y_PX)),
    };
    return PointUtils.androidPoint2Emr(moved, pageSize);
  });

  const pressureCount = await source.stroke.pressures.size();
  const sourcePressures = pressureCount > 0
    ? await source.stroke.pressures.getRange(0, pressureCount)
    : new Array(movedPoints.length).fill(1024);
  const pressures = sourcePressures.length === movedPoints.length
    ? sourcePressures
    : new Array(movedPoints.length).fill(sourcePressures[0] ?? 1024);

  await createNativeStroke({
    filePath,
    page,
    points: movedPoints,
    pressures,
    thickness: source.thickness ?? 2,
    layerNum: source.layerNum ?? 0,
    penColor: source.stroke.penColor ?? 0,
    penType: source.stroke.penType ?? 16,
  });
  await requireResult(PluginCommAPI.reloadFile(), 'reloadFile');

  return {filePath, page, sourceUuid: source.uuid ?? '(none)'};
}

export async function importBooxNativeStroke() {
  const {filePath, page, pageSize} = await currentDocumentContext();
  const points = samplesToEmr(
    BOOX_NATIVE_STROKE_FIXTURE.samples,
    pageSize,
  );
  const pressures = samplePressures(BOOX_NATIVE_STROKE_FIXTURE.samples);

  await createNativeStroke({
    filePath,
    page,
    points,
    pressures,
    thickness: 2,
    layerNum: 0,
    penColor: 0x00,
    penType: 16,
    userData: JSON.stringify({
      inkBridgeOrigin: 'boox-neoreader',
      sourceUuid: BOOX_NATIVE_STROKE_FIXTURE.sourceUuid,
    }),
  });
  await requireResult(PluginCommAPI.reloadFile(), 'reloadFile');

  return {
    filePath,
    page,
    sourceUuid: BOOX_NATIVE_STROKE_FIXTURE.sourceUuid,
    sampleCount: points.length,
  };
}

export async function applyBooxReturnTest() {
  const {filePath, page, pageSize} = await currentDocumentContext();
  if (page !== BOOX_RETURN_FIXTURE.sourcePageIndex) {
    throw new Error(
      `Open page ${BOOX_RETURN_FIXTURE.sourcePageIndex + 1} of the original annotated PDF before applying the BOOX return.`,
    );
  }

  let elements = await requireResult(
    PluginFileAPI.getElements(page, filePath),
    'getElements',
  );
  elements = elements ?? [];

  let modifiedCount = 0;
  for (const moved of BOOX_RETURN_FIXTURE.moved) {
    const target = elements.find(
      element => element?.uuid === moved.sourceUuid && element?.type === 0 && element?.stroke,
    );
    if (!target?.stroke) {
      throw new Error(`Could not find original Supernote stroke ${moved.sourceUuid}. Open the original annotated PDF copy.`);
    }

    const points = samplesToEmr(
      moved.samples,
      pageSize,
      BOOX_RETURN_FIXTURE.pdfToSupernoteNormalizedYOffset,
    );
    const pressures = samplePressures(moved.samples);
    const oldPointCount = await target.stroke.points.size();
    if (oldPointCount !== points.length) {
      throw new Error(
        `Moved stroke point count changed unexpectedly (${oldPointCount} != ${points.length}).`,
      );
    }

    const pointsOk = await target.stroke.points.setRange(
      0,
      points.length - 1,
      points,
    );
    if (!pointsOk) throw new Error('Could not update moved stroke points.');

    const pressureOk = await target.stroke.pressures.setRange(
      0,
      pressures.length - 1,
      pressures,
    );
    if (!pressureOk) throw new Error('Could not update moved stroke pressure data.');

    await requireResult(
      PluginFileAPI.modifyElements(filePath, page, [target]),
      'modifyElements',
    );
    modifiedCount += 1;
  }

  const deleteNums = BOOX_RETURN_FIXTURE.deleted
    .map(uuid => elements.find(element => element?.uuid === uuid)?.numInPage)
    .filter(numInPage => Number.isInteger(numInPage));
  if (deleteNums.length) {
    await requireResult(
      PluginFileAPI.deleteElements(filePath, page, deleteNums),
      'deleteElements',
    );
  }

  elements = await requireResult(
    PluginFileAPI.getElements(page, filePath),
    'getElements after delete',
  );
  const importedIds = new Set(
    (elements ?? [])
      .map(element => parseUserData(element?.userData))
      .filter(data => data?.inkBridgeOrigin === 'boox-neoreader-return')
      .map(data => data.sourceUuid),
  );

  let insertedCount = 0;
  for (const inserted of BOOX_RETURN_FIXTURE.inserted) {
    if (importedIds.has(inserted.sourceUuid)) continue;
    const points = samplesToEmr(
      inserted.samples,
      pageSize,
      BOOX_RETURN_FIXTURE.pdfToSupernoteNormalizedYOffset,
    );
    const pressures = samplePressures(inserted.samples);
    await createNativeStroke({
      filePath,
      page,
      points,
      pressures,
      thickness: inserted.thickness ?? 400,
      layerNum: 0,
      penColor: inserted.penColor ?? 0x00,
      penType: inserted.penType ?? 16,
      userData: JSON.stringify({
        inkBridgeOrigin: 'boox-neoreader-return',
        sourceUuid: inserted.sourceUuid,
      }),
    });
    insertedCount += 1;
  }

  await requireResult(PluginCommAPI.reloadFile(), 'reloadFile');
  return {
    filePath,
    page,
    modifiedCount,
    deletedCount: deleteNums.length,
    insertedCount,
  };
}

async function serializeSupernoteStroke(
  source,
  elementIndex,
  pageSize,
  page,
  expectedDocumentId = null,
) {
  const pointCount = await source.stroke.points.size();
  if (!pointCount) return null;

  const emrPoints = await source.stroke.points.getRange(0, pointCount);
  const pressureCount = await source.stroke.pressures.size();
  const sourcePressures = pressureCount > 0
    ? await source.stroke.pressures.getRange(0, pressureCount)
    : new Array(pointCount).fill(1024);
  const pressures = sourcePressures.length === pointCount
    ? sourcePressures
    : new Array(pointCount).fill(sourcePressures[0] ?? 1024);

  const maxPixelX = Math.max(1, pageSize.width - 1);
  const maxPixelY = Math.max(1, pageSize.height - 1);
  const samples = emrPoints.map((point, index) => {
    const pixel = PointUtils.emrPoint2Android(point, pageSize);
    return [
      Math.max(0, Math.min(1, pixel.x / maxPixelX)),
      Math.max(0, Math.min(1, pixel.y / maxPixelY)),
      Math.max(0, Math.min(4096, Math.round(pressures[index] ?? 1024))),
    ];
  });

  const sourceUuid = exportedStrokeIdentity(
    source.uuid,
    source.userData,
    expectedDocumentId,
  );
  return {
    sourceUuid,
    sourceKey: sourceUuid ?? `supernote-page-${page}-element-${elementIndex}`,
    elementIndex,
    layerNum: source.layerNum ?? 0,
    thickness: source.thickness ?? 2,
    penColor: source.stroke.penColor ?? 0x00,
    penType: source.stroke.penType ?? 16,
    userData: source.userData ?? null,
    samples,
  };
}

export async function collectCurrentSupernotePage(expectedDocumentId = null) {
  const {filePath, page, pageSize} = await currentDocumentContext();
  const elements = await requireResult(
    PluginFileAPI.getElements(page, filePath),
    'getElements',
  );

  const nativeStrokes = (elements ?? [])
    .map((element, elementIndex) => ({element, elementIndex}))
    .filter(({element}) => element?.type === 0 && element?.stroke);
  const strokes = [];
  let totalSamples = 0;
  for (const {element, elementIndex} of nativeStrokes) {
    const serialized = await serializeSupernoteStroke(
      element,
      elementIndex,
      pageSize,
      page,
      expectedDocumentId,
    );
    if (serialized) {
      totalSamples += serialized.samples.length;
      strokes.push(serialized);
    }
  }
  const slash = filePath.lastIndexOf('/');
  const sourceFileName = slash >= 0 ? filePath.slice(slash + 1) : filePath;
  const payload = {
    schemaVersion: 2,
    source: 'Supernote native annotated page',
    sourceDevice: 'Supernote Nomad',
    exportedAt: new Date().toISOString(),
    sourceFileName,
    pageIndex: page,
    pageSizePx: pageSize,
    pressureRange: [0, 4096],
    strokes,
  };

  return {
    filePath,
    payload,
    page,
    strokeCount: strokes.length,
    sampleCount: totalSamples,
  };
}

export async function collectCurrentVirtualSpread(
  representation,
  expectedFilePath = null,
  revalidateDocumentIdentity = null,
  nativeViewport = null,
) {
  const {filePath, page, pageSize} = await currentDocumentContext();
  requireSameDocumentPath(expectedFilePath, filePath);
  nativeViewportForVirtualSpread(
    representation,
    nativeViewport,
    page,
    pageSize,
  );
  const elements = await requireResult(
    PluginFileAPI.getElements(page, filePath),
    'getElements',
  );
  const nativeStrokes = (elements ?? [])
    .map((element, elementIndex) => ({element, elementIndex}))
    .filter(({element}) => element?.type === 0 && element?.stroke);
  const strokes = [];
  const identityTags = [];
  let totalSamples = 0;
  for (const {element, elementIndex} of nativeStrokes) {
    const retained = parseUserData(element.userData);
    if (retained?.inkBridgeOrigin === 'inkbridge-supernote-native') {
      if (
        retained.documentId !== representation.documentId ||
        typeof retained.sourceUuid !== 'string' ||
        !retained.sourceUuid.trim()
      ) {
        throw new Error(
          'A native stroke carries InkBridge identity metadata for another document.',
        );
      }
    } else if (retained?.inkBridgeOrigin === 'inkbridge-sync') {
      if (
        typeof retained.sourceUuid !== 'string' ||
        !retained.sourceUuid.trim()
      ) {
        throw new Error(
          'A synchronized native stroke has invalid InkBridge identity metadata.',
        );
      }
    } else {
      if (element.userData) {
        throw new Error(
          'A native stroke has non-InkBridge metadata, so InkBridge cannot safely assign it a stable identity.',
        );
      }
      if (typeof element.uuid !== 'string' || !element.uuid.trim()) {
        throw new Error(
          'A native stroke has no UUID that InkBridge can persist as its stable identity.',
        );
      }
      element.userData = JSON.stringify({
        inkBridgeOrigin: 'inkbridge-supernote-native',
        sourceUuid: element.uuid,
        documentId: representation.documentId,
      });
      identityTags.push(element);
    }
    const serialized = await serializeSupernoteStroke(
      element,
      elementIndex,
      pageSize,
      page,
      representation.documentId,
    );
    if (serialized) {
      totalSamples += serialized.samples.length;
      strokes.push(serialized);
    }
  }
  if (identityTags.length) {
    if (typeof revalidateDocumentIdentity !== 'function') {
      throw new Error(
        'InkBridge cannot persist stable stroke identities without revalidating the original document.',
      );
    }
    const currentBeforeIdentityWrite = await requireResult(
      PluginCommAPI.getCurrentFilePath(),
      'getCurrentFilePath before identity persistence',
    );
    requireSameDocumentPath(filePath, currentBeforeIdentityWrite);
    await revalidateDocumentIdentity();
    const currentAfterIdentityValidation = await requireResult(
      PluginCommAPI.getCurrentFilePath(),
      'getCurrentFilePath after identity validation',
    );
    requireSameDocumentPath(filePath, currentAfterIdentityValidation);
    await requireResult(
      PluginFileAPI.modifyElements(filePath, page, identityTags),
      'persist InkBridge stroke identities',
    );
  }
  const pages = buildVirtualSpreadSnapshot({
    representation,
    virtualPageIndex: page,
    nativeViewport,
    nativePageSize: pageSize,
    strokes,
  });
  return {
    filePath,
    payload: {
      schemaVersion: 1,
      sourceFileName: representation.sourceFileName,
      pages,
    },
    page,
    representedPageIndices: pages.map(snapshot => snapshot.pageIndex),
    strokeCount: pages.reduce(
      (count, snapshot) => count + snapshot.strokes.length,
      0,
    ),
    sampleCount: totalSamples,
  };
}

export async function exportCurrentSupernotePage() {
  const collected = await collectCurrentSupernotePage();
  const {payload, page, strokeCount, sampleCount} = collected;

  const compactJson = JSON.stringify(payload);
  const chunkCount = Math.ceil(compactJson.length / LOG_CHUNK_SIZE);
  for (let i = 0; i < chunkCount; i += 1) {
    const chunk = compactJson.slice(i * LOG_CHUNK_SIZE, (i + 1) * LOG_CHUNK_SIZE);
    console.log(`INKBRIDGE_EXPORT ${i + 1}/${chunkCount} ${chunk}`);
  }

  return {
    page,
    strokeCount,
    sampleCount,
    chunkCount,
  };
}

export default function App() {
  return (
    <View style={styles.root}>
      <Text style={styles.title}>InkBridge</Text>
      <Text style={styles.body}>
        InkBridge folder actions run directly from the NOTE/DOC toolbar without opening a plugin panel.
      </Text>
    </View>
  );
}

const styles = StyleSheet.create({
  root: {
    flex: 1,
    backgroundColor: '#ffffff',
    padding: 28,
    justifyContent: 'center',
  },
  title: {
    color: '#000000',
    fontSize: 28,
    fontWeight: '700',
    marginBottom: 20,
  },
  body: {
    color: '#000000',
    fontSize: 18,
    lineHeight: 28,
  },
});
