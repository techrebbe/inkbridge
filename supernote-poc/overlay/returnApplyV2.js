import {
  PluginCommAPI,
  PluginFileAPI,
  PointUtils,
} from 'sn-plugin-lib';
import {BOOX_RETURN_FIXTURE_V4 as BOOX_RETURN_FIXTURE} from './booxReturnFixtureV4';

const MOVED_INK_REVISION = 3;
const INSERTED_INK_REVISION = 4;

const MOVED_FALLBACKS = [
  {
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
  if (!count) return null;
  const points = await element.stroke.points.getRange(0, count);
  const maxPixelX = Math.max(1, pageSize.width - 1);
  const maxPixelY = Math.max(1, pageSize.height - 1);
  let minX = 1;
  let maxX = 0;
  let minY = 1;
  let maxY = 0;
  for (const point of points) {
    const pixel = PointUtils.emrPoint2Android(point, pageSize);
    const x = pixel.x / maxPixelX;
    const y = pixel.y / maxPixelY;
    minX = Math.min(minX, x);
    maxX = Math.max(maxX, x);
    minY = Math.min(minY, y);
    maxY = Math.max(maxY, y);
  }
  return {minX, maxX, minY, maxY, count};
}

async function candidateMatches(candidate, fallback, pageSize, expectedPointCount) {
  if (!candidate?.stroke || candidate?.type !== 0) return false;
  if ((candidate.thickness ?? null) !== fallback.thickness) return false;
  if ((candidate.stroke.penColor ?? null) !== fallback.penColor) return false;
  if ((candidate.stroke.penType ?? null) !== fallback.penType) return false;

  const bounds = await normalizedBounds(candidate, pageSize);
  if (!bounds) return false;
  if (expectedPointCount != null && bounds.count !== expectedPointCount) return false;

  const expected = fallback.bbox;
  return (
    inRange(bounds.minX, expected.minX) &&
    inRange(bounds.maxX, expected.maxX) &&
    inRange(bounds.minY, expected.minY) &&
    inRange(bounds.maxY, expected.maxY)
  );
}

async function findStroke(
  elements,
  sourceUuid,
  fallback,
  pageSize,
  expectedPointCount,
  label,
  allowAbsent = false,
) {
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

  const matches = [];
  for (const candidate of elements) {
    if (await candidateMatches(candidate, fallback, pageSize, expectedPointCount)) {
      matches.push(candidate);
    }
  }

  if (matches.length === 0 && allowAbsent) {
    console.log(`INKBRIDGE_RETURN_MATCH ${label} method=already-absent`);
    return null;
  }
  if (matches.length !== 1) {
    const nums = matches.map(item => item?.numInPage).join(',');
    throw new Error(
      `${label} geometry search expected exactly one match; found ${matches.length}` +
        (nums ? ` (numInPage=${nums})` : ''),
    );
  }

  const match = matches[0];
  console.log(
    `INKBRIDGE_RETURN_MATCH ${label} method=page-geometry-search numInPage=${match.numInPage}`,
  );
  return match;
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

function returnInkData(element) {
  const data = parseUserData(element?.userData);
  if (data?.inkBridgeOrigin !== 'boox-neoreader-return') return null;
  return data;
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

  const insertedUuidSet = new Set(
    BOOX_RETURN_FIXTURE.inserted.map(item => item.sourceUuid),
  );
  const priorReturnElements = elements.filter(element => {
    const data = parseUserData(element?.userData);
    return (
      data?.inkBridgeOrigin === 'boox-neoreader-return-moved' ||
      (data?.inkBridgeOrigin === 'boox-neoreader-return' && insertedUuidSet.has(data.sourceUuid))
    );
  });
  const isRepairRun = priorReturnElements.length > 0;

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
        isRepairRun,
      ),
    );
  }

  console.log(
    `INKBRIDGE_RETURN_STAGE resolved moved=${movedTargets.length} deleted=${deletedTargets.filter(Boolean).length} repair=${isRepairRun}`,
  );

  let modifiedCount = 0;
  let movedReplacedCount = 0;
  let movedAlreadyCorrectCount = 0;
  for (let index = 0; index < BOOX_RETURN_FIXTURE.moved.length; index += 1) {
    const moved = BOOX_RETURN_FIXTURE.moved[index];
    const target = movedTargets[index];
    const targetData = parseUserData(target?.userData);
    const isAlreadyCorrect =
      targetData?.inkBridgeOrigin === 'boox-neoreader-return-moved' &&
      targetData?.sourceUuid === moved.sourceUuid &&
      targetData?.inkBridgeRevision === MOVED_INK_REVISION;
    if (isAlreadyCorrect) {
      movedAlreadyCorrectCount += 1;
      continue;
    }

    const points = samplesToEmr(
      moved.samples,
      pageSize,
      BOOX_RETURN_FIXTURE.pdfToSupernoteNormalizedYOffset,
    );
    const pressures = samplePressures(moved.samples);

    await createNativeStroke({
      filePath,
      page,
      points,
      pressures,
      thickness: target.thickness ?? 700,
      layerNum: target.layerNum ?? 0,
      penColor: target.stroke.penColor ?? 0x00,
      penType: target.stroke.penType ?? 10,
      userData: JSON.stringify({
        inkBridgeOrigin: 'boox-neoreader-return-moved',
        sourceUuid: moved.sourceUuid,
        inkBridgeRevision: MOVED_INK_REVISION,
      }),
    });
    const elementsWithReplacement = (await requireResult(
      PluginFileAPI.getElements(page, filePath),
      'getElements after moved-stroke replacement insert',
    )) ?? [];
    const supersededTarget = elementsWithReplacement.find(element => {
      if (target.uuid && element?.uuid === target.uuid) return true;
      const data = parseUserData(element?.userData);
      return (
        data?.inkBridgeOrigin === 'boox-neoreader-return-moved' &&
        data?.sourceUuid === moved.sourceUuid &&
        data?.inkBridgeRevision !== MOVED_INK_REVISION
      );
    });
    if (!Number.isInteger(supersededTarget?.numInPage)) {
      throw new Error('Could not locate the superseded moved stroke after inserting its replacement.');
    }
    await requireResult(
      PluginFileAPI.deleteElements(filePath, page, [supersededTarget.numInPage]),
      'delete superseded moved stroke',
    );
    movedReplacedCount += 1;
    modifiedCount += 1;
  }
  console.log(
    `INKBRIDGE_RETURN_STAGE modified=${modifiedCount} movedReplaced=${movedReplacedCount} movedAlreadyCorrect=${movedAlreadyCorrectCount}`,
  );

  const deleteNums = deletedTargets
    .filter(Boolean)
    .map(target => target?.numInPage)
    .filter(numInPage => Number.isInteger(numInPage));
  if (deleteNums.length) {
    await requireResult(
      PluginFileAPI.deleteElements(filePath, page, deleteNums),
      'deleteElements',
    );
  }
  console.log(`INKBRIDGE_RETURN_STAGE deleted=${deleteNums.length}`);

  elements = (await requireResult(
    PluginFileAPI.getElements(page, filePath),
    'getElements after source updates',
  )) ?? [];

  const existingBySource = new Map();
  for (const element of elements) {
    const data = returnInkData(element);
    if (!data || !insertedUuidSet.has(data.sourceUuid)) continue;
    const list = existingBySource.get(data.sourceUuid) ?? [];
    list.push({element, data});
    existingBySource.set(data.sourceUuid, list);
  }

  const alreadyCorrect = BOOX_RETURN_FIXTURE.inserted.every(inserted => {
    const matches = existingBySource.get(inserted.sourceUuid) ?? [];
    return matches.length === 1 && matches[0].data.inkBridgeRevision === INSERTED_INK_REVISION;
  });

  let replacedCount = 0;
  let insertedCount = 0;
  if (!alreadyCorrect) {
    const replaceNums = [];
    for (const matches of existingBySource.values()) {
      for (const {element} of matches) {
        if (Number.isInteger(element?.numInPage)) replaceNums.push(element.numInPage);
      }
    }
    if (replaceNums.length) {
      await requireResult(
        PluginFileAPI.deleteElements(filePath, page, replaceNums),
        'delete incorrect BOOX return strokes',
      );
      replacedCount = replaceNums.length;
    }
    console.log(`INKBRIDGE_RETURN_STAGE replaced=${replacedCount}`);

    for (const inserted of BOOX_RETURN_FIXTURE.inserted) {
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
          inkBridgeRevision: INSERTED_INK_REVISION,
        }),
      });
      insertedCount += 1;
    }
  }
  console.log(
    `INKBRIDGE_RETURN_STAGE inserted=${insertedCount} alreadyCorrect=${alreadyCorrect}`,
  );

  await requireResult(PluginCommAPI.reloadFile(), 'reloadFile');
  return {
    filePath,
    page,
    modifiedCount,
    movedReplacedCount,
    movedAlreadyCorrectCount,
    deletedCount: deleteNums.length,
    replacedCount,
    insertedCount,
    alreadyCorrect,
  };
}
