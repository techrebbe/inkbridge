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
  completedVirtualSpreadDelivery,
  finishVirtualSpreadStep,
  nativeViewportMap,
  planVirtualSpreadDelivery,
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
  if (representation) {
    requireSameNativeViewport(
      nativeViewport,
      await currentNativeViewport(
        native,
        filePath,
        representation,
        nativeDescriptor,
      ),
    );
  }
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
      const currentVirtualPageIndex = requirePluginResult(
        await PluginCommAPI.getCurrentPageNum(),
        'getCurrentPageNum before Virtual Spread delivery staging',
      );
      const plan = planVirtualSpreadDelivery(
        manifest,
        representation,
        currentVirtualPageIndex,
        delivery.virtualSpreadProgress,
      );
      if (plan.complete) {
        return completedVirtualSpreadDelivery(
          manifest,
          plan.steps,
          plan.progress,
        );
      }
      if (!plan.manifest) {
        return {
          status: 'pending',
          acknowledge: false,
          message:
            `Open Virtual Spread page ${plan.nextPage + 1}, then tap Apply InkBridge Sync again.`,
        };
      }
      const nativeViewport = await currentNativeViewport(
        native,
        filePath,
        representation,
        nativeDescriptor,
      );
      const applied = await applyVirtualSpreadManifest(
        plan.manifest,
        representation,
        filePath,
        nativeViewportMap(nativeViewport),
        false,
      );
      // Do not let our own redraw invalidate the generation fence. Native
      // writes finish first; only a still-matching viewport may commit the
      // durable step. A failed fence leaves an idempotent retry, not a skip.
      const progress = await finishVirtualSpreadStep({
        expectedViewport: nativeViewport,
        readCurrentViewport: () => currentNativeViewport(
          native,
          filePath,
          representation,
          nativeDescriptor,
        ),
        recordProgress: async () => parseNativeJson(
          await native.recordVirtualSpreadStepApplied(
            filePath,
            delivery.deliveryId,
            manifest.manifestId,
            plan.stepId,
            JSON.stringify({
              operationCount: plan.manifest.operations.length,
              added: applied.added,
              updated: applied.updated,
              deleted: applied.deleted,
              skipped: applied.skipped,
            }),
            nativeDescriptor,
          ),
          'recordVirtualSpreadStepApplied',
        ),
        reload: async () => requirePluginResult(
          await PluginCommAPI.reloadFile(),
          'reloadFile after Virtual Spread progress commit',
        ),
      });
      const completed = completedVirtualSpreadDelivery(
        manifest,
        plan.steps,
        progress,
      );
      if (!completed.complete) {
        const nextPage = plan.steps.find(
          step => !progress.completedStepIds.includes(step.id),
        ).page;
        return {
          status: 'pending',
          acknowledge: false,
          message:
            `Applied this spread. Open Virtual Spread page ${nextPage + 1}, then tap Apply InkBridge Sync again.`,
        };
      }
      return completed;
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
