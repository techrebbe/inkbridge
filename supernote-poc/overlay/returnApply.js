import {
  PluginCommAPI,
  PluginFileAPI,
  PointUtils,
} from 'sn-plugin-lib';
import {BOOX_RETURN_FIXTURE} from './booxReturnFixture';

const MOVED_FALLBACKS = [
  {
    numInPage: 0,
    thickness: 700,
    penColor: 0x00,
    penType: 10,
    bbox: {
      minX: [0.10, 0.14],
      maxX: [0.86, 0.90],
      minY: [0.39, 0.43],
      maxY: [0.40, 0.44],
    },
  },
];

const DELETED_FALLBACKS = [
  {
    numInPage: 5,
    thickness: 400,
    penColor: 0xC9,
    penType: 10,
    bbox: {
      minX: [0.42, 0.47],
      maxX: [0.88, 0.93],
      minY: [0.72, 0.78],
      maxY: [0.74, 0.79],
    },
  },
];

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
    const correctedY = Math.max(0, Math.min(1, normalizedY + normalizedYOffset));
    const pixel = {
      x: Math.max(0, Math.min(pageSize.width - 1, normalizedX * (pageSize.width - 1))),
      y: Math.max(0, Math.min(pageSize.height - 1, correctedY * (pageSize.height - 1))),
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

function inRange(value, range) {
  return value >= range[0] && value <= range[1];
}

async function normalizedBounds(element, pageSize) {
  const count = await element.stroke.points.size();
  if (!count) throw new Error('Fallback candidate has no stroke points.');
  const points = await element.stroke.points.getRange(0, count);
  const maxX = Math.max(1, pageSize.width - 1);
  const maxY = Math.max(1, pageSize.height - 1);
  let minX = 1;
  let maxNormX = 0;
  let minY = 1;
  let maxNormY = 0;
  for (const point of points) {
    const pixel = PointUtils.emrPoint2Android(point, pageSize);
    const x = pixel.x / maxX;
    const y = pixel.y / maxY;
    minX = Math.min(minX, x);
    maxNormX = Math.max(maxNormX, x);
    minY = Math.min(minY, y);
    maxNormY = Math.max(maxNormY, y);
  }
  return {minX, maxX: maxNormX, minY, maxY: maxNormY, count};
}

async function validateFallback(candidate, fallback, pageSize, expectedPointCount, label) {
  if (!candidate?.stroke || candidate?.type !== 0) {
    throw new Error(`${label} fallback is not a native stroke.`);
  }
  if ((candidate.thickness ?? null) !== fallback.thickness) {
    throw new Error(`${label} fallback thickness mismatch.`);
  }
  if ((candidate.stroke.penColor ?? null) !== fallback.penColor) {
    throw new Error(`${label} fallback color mismatch.`);
  }
  if ((candidate.stroke.penType ?? null) !== fallback.penType) {
    throw new Error(`${label} fallback pen-type mismatch.`);
  }

  const bounds = await normalizedBounds(candidate, pageSize);
  if (expectedPointCount != null && bounds.count !== expectedPointCount) {
    throw new Error(
      `${label} fallback point-count mismatch (${bounds.count} != ${expectedPointCount}).`,
    );
  }

  const expected = fallback.bbox;
  if (
    !inRange(bounds.minX, expected.minX) ||
    !inRange(bounds.maxX, expected.maxX) ||
    !inRange(bounds.minY, expected.minY) ||
    !inRange(bounds.maxY, expected.maxY)
  ) {
    throw new Error(
      `${label} fallback geometry mismatch: ` +
        JSON.stringify({
          minX: bounds.minX,
          maxX: bounds.maxX,
          minY: bounds.minY,
          maxY: bounds.maxY,
        }),
    );
  }
  return candidate;
}

async function findStroke(elements, sourceUuid, fallback, pageSize, expectedPointCount, label) {
  const exact = elements.find(
    element =>
      element?.type === 0 &&
      element?.stroke &&
      (element.uuid === sourceUuid || parseUserData(element.userData)?.sourceUuid === sourceUuid),
  );
  if (exact) {
    console.log(`INKBRIDGE_RETURN_MATCH ${label} method=identity numInPage=${exact.numInPage}`);
    return exact;
  }

  const byNum = elements.find(
    element => element?.numInPage === fallback.numInPage && element?.type === 0 && element?.stroke,
  );
  const candidate = byNum ?? elements[fallback.numInPage];
  const validated = await validateFallback(
    candidate,
    fallback,
    pageSize,
    expectedPointCount,
    label,
  );
  console.log(
    `INKBRIDGE_RETURN_MATCH ${label} method=geometry-fallback numInPage=${validated.numInPage}`,
  );
  return validated;
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

  const pointsOk = await target.stroke.points.setRange(0, points.length - 1, points);
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

export async function applyBooxReturnTest() {
  const {filePath, page, pageSize} = await currentDocumentContext();
  if (page !== BOOX_RETURN_FIXTURE.sourcePageIndex) {
    throw new Error(
      `Open page ${BOOX_RETURN_FIXTURE.sourcePageIndex + 1} of the original annotated PDF before applying the BOOX return.`,
    );
  }

  let elements = (await requireResult(
    PluginFileAPI.getElements(page, filePath),
    'getElements',
  )) ?? [];

  const movedTargets = [];
  for (let index = 0; index < BOOX_RETURN_FIXTURE.moved.length; index += 1) {
    const moved = BOOX_RETURN_FIXTURE.moved[index];
    const fallback = MOVED_FALLBACKS[index];
    if (!fallback) throw new Error(`Missing moved-stroke fallback ${index}.`);
    movedTargets.push(
      await findStroke(
        elements,
        moved.sourceUuid,
        fallback,
        pageSize,
        moved.samples.length,
        `moved-${index}`,
      ),
    );
  }

  const deletedTargets = [];
  for (let index = 0; index < BOOX_RETURN_FIXTURE.deleted.length; index += 1) {
    const sourceUuid = BOOX_RETURN_FIXTURE.deleted[index];
    const fallback = DELETED_FALLBACKS[index];
    if (!fallback) throw new Error(`Missing deleted-stroke fallback ${index}.`);
    deletedTargets.push(
      await findStroke(
        elements,
        sourceUuid,
        fallback,
        pageSize,
        null,
        `deleted-${index}`,
      ),
    );
  }

  let modifiedCount = 0;
  for (let index = 0; index < BOOX_RETURN_FIXTURE.moved.length; index += 1) {
    const moved = BOOX_RETURN_FIXTURE.moved[index];
    const target = movedTargets[index];
    const points = samplesToEmr(
      moved.samples,
      pageSize,
      BOOX_RETURN_FIXTURE.pdfToSupernoteNormalizedYOffset,
    );
    const pressures = samplePressures(moved.samples);

    const pointsOk = await target.stroke.points.setRange(0, points.length - 1, points);
    if (!pointsOk) throw new Error('Could not update moved stroke points.');
    const pressureOk = await target.stroke.pressures.setRange(
      0,
      pressures.length - 1,
      pressures,
    );
    if (!pressureOk) throw new Error('Could not update moved stroke pressure data.');

    target.userData = JSON.stringify({
      inkBridgeOrigin: 'boox-neoreader-return-moved',
      sourceUuid: moved.sourceUuid,
    });
    await requireResult(
      PluginFileAPI.modifyElements(filePath, page, [target]),
      'modifyElements',
    );
    modifiedCount += 1;
  }

  const deleteNums = deletedTargets
    .map(target => target?.numInPage)
    .filter(numInPage => Number.isInteger(numInPage));
  if (deleteNums.length) {
    await requireResult(
      PluginFileAPI.deleteElements(filePath, page, deleteNums),
      'deleteElements',
    );
  }

  elements = (await requireResult(
    PluginFileAPI.getElements(page, filePath),
    'getElements after delete',
  )) ?? [];
  const importedIds = new Set(
    elements
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
