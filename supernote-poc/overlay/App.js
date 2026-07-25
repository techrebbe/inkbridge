import React from 'react';
import {StyleSheet, Text, View} from 'react-native';
import {
  PluginCommAPI,
  PluginFileAPI,
  PointUtils,
} from 'sn-plugin-lib';

const OFFSET_X_PX = 80;
const OFFSET_Y_PX = 50;

async function requireResult(promise, label) {
  const response = await promise;
  if (!response?.success) {
    throw new Error(response?.error?.message ?? `${label} failed`);
  }
  return response.result;
}

export async function duplicateFirstStroke() {
  const filePath = await requireResult(
    PluginCommAPI.getCurrentFilePath(),
    'getCurrentFilePath',
  );
  const page = await requireResult(
    PluginCommAPI.getCurrentPageNum(),
    'getCurrentPageNum',
  );
  const elements = await requireResult(
    PluginFileAPI.getElements(page, filePath),
    'getElements',
  );

  const source = (elements ?? []).find(element => element?.type === 0 && element?.stroke);
  if (!source?.stroke) {
    throw new Error('No handwritten stroke found on the current page. Write one first, then run InkBridge Test again.');
  }

  const pageSize = await requireResult(
    PluginFileAPI.getPageSize(filePath, page),
    'getPageSize',
  );

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
  const pressures = pressureCount > 0
    ? await source.stroke.pressures.getRange(0, pressureCount)
    : new Array(movedPoints.length).fill(1024);

  const target = await requireResult(
    PluginCommAPI.createElement(0),
    'createElement',
  );
  if (!target?.stroke) {
    throw new Error('createElement returned a stroke without stroke accessors.');
  }

  target.layerNum = source.layerNum ?? 0;
  target.thickness = source.thickness ?? 2;
  target.stroke.penColor = source.stroke.penColor ?? 0;
  target.stroke.penType = source.stroke.penType ?? 16;

  const pointsOk = await target.stroke.points.setRange(
    0,
    movedPoints.length - 1,
    movedPoints,
  );
  if (!pointsOk) throw new Error('Could not write duplicate stroke points.');

  const normalizedPressures = pressures.length === movedPoints.length
    ? pressures
    : new Array(movedPoints.length).fill(pressures[0] ?? 1024);
  const pressureOk = await target.stroke.pressures.setRange(
    0,
    normalizedPressures.length - 1,
    normalizedPressures,
  );
  if (!pressureOk) throw new Error('Could not write duplicate stroke pressure data.');

  await requireResult(
    PluginFileAPI.insertElements(filePath, page, [target]),
    'insertElements',
  );
  await requireResult(PluginCommAPI.reloadFile(), 'reloadFile');

  return {filePath, page, sourceUuid: source.uuid ?? '(none)'};
}

// This component is retained as a harmless fallback, but the toolbar button is
// intentionally registered with showType: 0 so normal use never leaves NOTE/DOC.
export default function App() {
  return (
    <View style={styles.root}>
      <Text style={styles.title}>InkBridge Test</Text>
      <Text style={styles.body}>
        This proof now runs directly from the NOTE/DOC toolbar without opening a plugin panel.
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
