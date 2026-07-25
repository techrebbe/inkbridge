import React from 'react';
import {StyleSheet, Text, View} from 'react-native';
import {
  PluginCommAPI,
  PluginFileAPI,
  PointUtils,
} from 'sn-plugin-lib';
import {BOOX_NATIVE_STROKE_FIXTURE} from './booxFixture';

const OFFSET_X_PX = 80;
const OFFSET_Y_PX = 50;

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

  const points = BOOX_NATIVE_STROKE_FIXTURE.samples.map(([normalizedX, normalizedY]) => {
    const pixel = {
      x: Math.max(0, Math.min(pageSize.width - 1, normalizedX * (pageSize.width - 1))),
      y: Math.max(0, Math.min(pageSize.height - 1, normalizedY * (pageSize.height - 1))),
    };
    return PointUtils.androidPoint2Emr(pixel, pageSize);
  });

  // BOOX reports pressure against maxPressure=4095; Supernote documents 0..4096.
  // Preserve the measured pressure samples directly for this interoperability proof.
  const pressures = BOOX_NATIVE_STROKE_FIXTURE.samples.map(([, , pressure]) =>
    Math.max(0, Math.min(4096, Math.round(pressure))),
  );

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

// This component is retained as a harmless fallback. Both proof buttons are
// registered with showType: 0 so normal use never leaves NOTE/DOC.
export default function App() {
  return (
    <View style={styles.root}>
      <Text style={styles.title}>InkBridge Test</Text>
      <Text style={styles.body}>
        InkBridge proof actions run directly from the NOTE/DOC toolbar without opening a plugin panel.
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
