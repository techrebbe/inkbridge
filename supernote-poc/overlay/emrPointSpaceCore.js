function positiveRange(value, label) {
  if (!Number.isInteger(value) || value <= 0) {
    throw new Error(`${label} must be a positive integer.`);
  }
  return value;
}

export function elementEmrRange(element) {
  if (!element || typeof element !== 'object' || Array.isArray(element)) {
    throw new Error('Native element EMR metadata is unavailable.');
  }
  return {
    maxX: positiveRange(element.maxX, 'native element maxX'),
    maxY: positiveRange(element.maxY, 'native element maxY'),
  };
}

function clampUnit(value) {
  if (!Number.isFinite(value)) {
    throw new Error('Native EMR coordinate is not finite.');
  }
  return Math.max(0, Math.min(1, value));
}

export function normalizedEmrPoint(point, range) {
  const validated = elementEmrRange(range);
  return [
    clampUnit(1 - point.y / validated.maxY),
    clampUnit(point.x / validated.maxX),
  ];
}

export function emrPointFromSample(
  sample,
  range,
  normalizedYOffset = 0,
) {
  if (!Array.isArray(sample) || sample.length < 2) {
    throw new Error('Stroke sample must contain x and y coordinates.');
  }
  const validated = elementEmrRange(range);
  const normalizedX = clampUnit(sample[0]);
  const normalizedY = clampUnit(sample[1] + normalizedYOffset);
  return {
    x: Math.round(normalizedY * validated.maxX),
    y: Math.round((1 - normalizedX) * validated.maxY),
  };
}

export function commonElementEmrRange(elements) {
  let common = null;
  for (const element of elements ?? []) {
    if (element?.type !== 0 || !element?.stroke) continue;
    const current = elementEmrRange(element);
    if (!common) {
      common = current;
      continue;
    }
    if (current.maxX !== common.maxX || current.maxY !== common.maxY) {
      throw new Error(
        'Native strokes on the page disagree about their EMR coordinate range.',
      );
    }
  }
  return common;
}

export function requireEmrRangeForInsertion(range) {
  if (!range) {
    throw new Error(
      'Virtual Spread cannot insert ink on a page without native EMR range authority.',
    );
  }
  return elementEmrRange(range);
}
