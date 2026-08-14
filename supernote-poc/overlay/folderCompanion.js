import {NativeModules} from 'react-native';
import {NativeUIUtils, PluginCommAPI} from 'sn-plugin-lib';
import {collectCurrentSupernotePage} from './App';
import {applyManifest} from './manifestApply';
import {
  describeFolderResult,
  parseNativeJson,
  processManifestDelivery,
  revalidateCollectedDocument,
  requirePluginResult,
  requireSameDocumentId,
} from './folderCompanionCore';

const {InkBridgeFolderModule} = NativeModules;

function requireNativeModule() {
  if (!InkBridgeFolderModule) {
    throw new Error(
      'InkBridge native folder support is unavailable. Reinstall the complete InkBridge plugin package.',
    );
  }
  return InkBridgeFolderModule;
}

async function currentFilePath() {
  return requirePluginResult(
    await PluginCommAPI.getCurrentFilePath(),
    'getCurrentFilePath',
  );
}

export async function publishCurrentPageExport() {
  const native = requireNativeModule();
  const filePath = await currentFilePath();
  const identity = parseNativeJson(
    await native.getDocumentIdentity(filePath),
    'getDocumentIdentity',
  );
  const collected = await collectCurrentSupernotePage();
  await revalidateCollectedDocument(filePath, async () => collected.filePath);
  await revalidateCollectedDocument(collected.filePath, currentFilePath);
  const result = parseNativeJson(
    await native.publishPageExport(
      collected.filePath,
      identity.documentId,
      JSON.stringify(collected.payload),
    ),
    'publishPageExport',
  );
  return {
    ...result,
    page: collected.page,
    strokeCount: collected.strokeCount,
    sampleCount: collected.sampleCount,
  };
}

export async function applyNextFolderManifest() {
  const native = requireNativeModule();
  const filePath = await currentFilePath();
  const delivery = parseNativeJson(
    await native.loadNextManifest(filePath),
    'loadNextManifest',
  );
  return processManifestDelivery({
    delivery,
    validate: async currentDelivery => {
      const revalidated = parseNativeJson(
        await native.validateDocumentIdentity(
          filePath,
          currentDelivery.documentId,
        ),
        'validateDocumentIdentity',
      );
      requireSameDocumentId(currentDelivery.documentId, revalidated.documentId);
    },
    apply: manifest => applyManifest(manifest, filePath, true),
    acknowledge: async ({deliveryId, manifestId, applied}) =>
      parseNativeJson(
        await native.acknowledgeManifest(
          filePath,
          deliveryId,
          manifestId,
          JSON.stringify(applied),
        ),
        'acknowledgeManifest',
      ),
    recordFailure: ({deliveryId, message}) =>
      native.recordManifestFailure(filePath, deliveryId, message),
  });
}

export async function getFolderStatus() {
  const native = requireNativeModule();
  const filePath = await currentFilePath();
  return parseNativeJson(await native.getStatus(filePath), 'getStatus');
}

export async function showFolderResult(result) {
  const message = describeFolderResult(result);
  console.log(`INKBRIDGE_FOLDER_STATUS status=${result?.status || 'error'} ${message}`);
  await NativeUIUtils.showRattaDialog(
    message,
    'Close',
    'OK',
    result?.status !== 'error' && result?.status !== 'conflict',
  );
}
