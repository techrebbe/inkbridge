import {NativeModules} from 'react-native';
import {NativeUIUtils, PluginCommAPI, PluginFileAPI} from 'sn-plugin-lib';
import {
  collectCurrentSupernotePage,
  collectCurrentVirtualSpread,
} from './App';
import {applyManifest, applyVirtualSpreadManifest} from './manifestApply';
import {
  describeFolderResult,
  parseNativeJson,
  processManifestDelivery,
  revalidateCollectedDocument,
  requirePluginResult,
  requireSameDocumentId,
  requireSameDocumentPath,
} from './folderCompanionCore';
import {
  nativeViewportMap,
  requireNativeViewportResult,
  requireSameNativeViewport,
} from './nativeViewportProviderCore';
import {
  fixtureForOpenPath,
  fixtureNativeDescriptor,
} from './virtualSpreadFixture';

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

async function currentNativeViewport(
  native,
  filePath,
  representation,
  nativeDescriptor,
) {
  const pathBefore = await currentFilePath();
  requireSameDocumentPath(filePath, pathBefore);
  const virtualPageIndex = requirePluginResult(
    await PluginCommAPI.getCurrentPageNum(),
    'getCurrentPageNum before native viewport request',
  );
  const nativePageSize = requirePluginResult(
    await PluginFileAPI.getPageSize(filePath, virtualPageIndex),
    'getPageSize before native viewport request',
  );
  const result = requireNativeViewportResult(
    parseNativeJson(
      await native.getNativeViewport(
        filePath,
        nativeDescriptor,
        virtualPageIndex,
        nativePageSize.width,
        nativePageSize.height,
      ),
      'getNativeViewport',
    ),
    representation,
    virtualPageIndex,
    nativePageSize,
  );
  const pathAfter = await currentFilePath();
  requireSameDocumentPath(filePath, pathAfter);
  return result;
}

export async function publishCurrentPageExport() {
  const native = requireNativeModule();
  const filePath = await currentFilePath();
  const representation = fixtureForOpenPath(filePath);
  const nativeDescriptor = representation
    ? fixtureNativeDescriptor(representation)
    : '';
  const identity = parseNativeJson(
    await native.getDocumentIdentity(filePath, nativeDescriptor),
    'getDocumentIdentity',
  );
  const nativeViewport = representation
    ? await currentNativeViewport(
        native,
        filePath,
        representation,
        nativeDescriptor,
      )
    : null;
  const collected = representation
    ? await collectCurrentVirtualSpread(
        representation,
        filePath,
        async () => {
          const revalidated = parseNativeJson(
            await native.validateDocumentIdentity(
              filePath,
              identity.documentId,
              nativeDescriptor,
            ),
            'validateDocumentIdentity before identity persistence',
          );
          requireSameDocumentId(identity.documentId, revalidated.documentId);
          requireSameNativeViewport(
            nativeViewport,
            await currentNativeViewport(
              native,
              filePath,
              representation,
              nativeDescriptor,
            ),
          );
        },
        nativeViewport.descriptor,
      )
    : await collectCurrentSupernotePage(identity.documentId);
  await revalidateCollectedDocument(filePath, async () => collected.filePath);
  await revalidateCollectedDocument(collected.filePath, currentFilePath);
  const result = parseNativeJson(
    await native.publishPageExport(
      collected.filePath,
      identity.documentId,
      JSON.stringify(collected.payload),
      nativeDescriptor,
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
  const representation = fixtureForOpenPath(filePath);
  const nativeDescriptor = representation
    ? fixtureNativeDescriptor(representation)
    : '';
  const delivery = parseNativeJson(
    await native.loadNextManifest(filePath, nativeDescriptor),
    'loadNextManifest',
  );
  return processManifestDelivery({
    delivery,
    validate: async currentDelivery => {
      const revalidated = parseNativeJson(
        await native.validateDocumentIdentity(
          filePath,
          currentDelivery.documentId,
          nativeDescriptor,
        ),
        'validateDocumentIdentity',
      );
      requireSameDocumentId(currentDelivery.documentId, revalidated.documentId);
    },
    apply: async manifest => {
      if (!representation) return applyManifest(manifest, filePath, true);
      const nativeViewport = await currentNativeViewport(
        native,
        filePath,
        representation,
        nativeDescriptor,
      );
      return applyVirtualSpreadManifest(
        manifest,
        representation,
        filePath,
        nativeViewportMap(nativeViewport),
      );
    },
    acknowledge: async ({deliveryId, manifestId, applied}) =>
      parseNativeJson(
        await native.acknowledgeManifest(
          filePath,
          deliveryId,
          manifestId,
          JSON.stringify(applied),
          nativeDescriptor,
        ),
        'acknowledgeManifest',
      ),
    recordFailure: ({deliveryId, message}) =>
      native.recordManifestFailure(
        filePath,
        deliveryId,
        message,
        nativeDescriptor,
      ),
  });
}

export async function getFolderStatus() {
  const native = requireNativeModule();
  const filePath = await currentFilePath();
  const representation = fixtureForOpenPath(filePath);
  const nativeDescriptor = representation
    ? fixtureNativeDescriptor(representation)
    : '';
  return parseNativeJson(
    await native.getStatus(filePath, nativeDescriptor),
    'getStatus',
  );
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
