import assert from 'node:assert/strict';
import test from 'node:test';

import {
  commonElementEmrRange,
  elementEmrRange,
  emrPointFromSample,
  insertionEmrRange,
  normalizedEmrPoint,
  requireEmrRangeForInsertion,
} from '../overlay/emrPointSpaceCore.js';

const virtualSpreadRange = {maxX: 15819, maxY: 21098};

test('Virtual Spread EMR metadata maps the hardware stroke into page space', () => {
  const normalized = normalizedEmrPoint(
    {x: 9197, y: 17991},
    virtualSpreadRange,
  );

  assert.ok(Math.abs(normalized[0] - 0.14726514361550858) < 1e-15);
  assert.ok(Math.abs(normalized[1] - 0.5813894683608319) < 1e-15);
});

test('Virtual Spread EMR conversion round-trips at native integer precision', () => {
  const source = {x: 8543, y: 17199};
  const normalized = normalizedEmrPoint(source, virtualSpreadRange);

  assert.deepEqual(
    emrPointFromSample(normalized, virtualSpreadRange),
    source,
  );
});

test('a page must expose one consistent EMR range', () => {
  assert.deepEqual(
    commonElementEmrRange([
      {type: 0, stroke: {}, ...virtualSpreadRange},
      {type: 0, stroke: {}, ...virtualSpreadRange},
      {type: 600, maxX: 1, maxY: 1},
    ]),
    virtualSpreadRange,
  );
  assert.throws(
    () => commonElementEmrRange([
      {type: 0, stroke: {}, ...virtualSpreadRange},
      {type: 0, stroke: {}, maxX: 11864, maxY: 15819},
    ]),
    /disagree/,
  );
});

test('deletion-only work does not require a common insertion range', () => {
  const mixedRanges = [
    {type: 0, stroke: {}, ...virtualSpreadRange},
    {type: 0, stroke: {}, maxX: 11864, maxY: 15819},
  ];
  assert.equal(
    insertionEmrRange(mixedRanges, [
      {operation: {type: 'delete_stroke'}},
    ]),
    null,
  );
  assert.throws(
    () => insertionEmrRange(mixedRanges, [
      {operation: {type: 'upsert_stroke'}},
    ]),
    /disagree/,
  );
});

test('invalid EMR metadata fails closed', () => {
  assert.throws(
    () => elementEmrRange({maxX: 0, maxY: 21098}),
    /maxX.*positive integer/,
  );
});

test('Virtual Spread insertion without native EMR range authority fails closed', () => {
  assert.throws(
    () => requireEmrRangeForInsertion(null),
    /without native EMR range authority/,
  );
  assert.deepEqual(
    requireEmrRangeForInsertion(virtualSpreadRange),
    virtualSpreadRange,
  );
});
