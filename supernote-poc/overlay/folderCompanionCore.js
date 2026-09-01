export function parseNativeJson(value, operation) {
  if (typeof value !== 'string' || !value.trim()) {
    throw new Error(`${operation} returned no result.`);
  }
  try {
    return JSON.parse(value);
  } catch (error) {
    throw new Error(`${operation} returned invalid JSON: ${String(error)}`);
  }
}

export function requirePluginResult(response, operation) {
  if (!response?.success) {
    throw new Error(response?.error?.message || `${operation} failed.`);
  }
  if (response.result === undefined || response.result === null) {
    throw new Error(`${operation} returned no result.`);
  }
  return response.result;
}

export function requireSameDocumentPath(expected, actual) {
  if (expected && actual !== expected) {
    throw new Error(
      'The open document changed while InkBridge was processing its update. Reopen the original document and retry.',
    );
  }
  return actual;
}

export function requireSameDocumentId(expected, actual) {
  if (!expected || actual !== expected) {
    throw new Error(
      'The PDF content changed while InkBridge was preparing its update. Reopen the intended document and retry.',
    );
  }
  return actual;
}

export async function revalidateCollectedDocument(expected, readCurrentPath) {
  return requireSameDocumentPath(expected, await readCurrentPath());
}

export function requireCompatibleTargetFileName(
  targetNames,
  openName,
  stableIdentityValidated = false,
) {
  if (
    !stableIdentityValidated &&
    targetNames.length &&
    !targetNames.includes(openName)
  ) {
    throw new Error(
      `This sync package targets ${targetNames.join(', ')}, but ${openName} is open.`,
    );
  }
}

export async function processManifestDelivery({
  delivery,
  validate,
  apply,
  acknowledge,
  recordFailure,
}) {
  if (!delivery.deliveryId) return delivery;
  try {
    if (validate) await validate(delivery);
    const applied = await apply(delivery.manifest);
    if (applied?.acknowledge === false) return applied;
    const acknowledged = await acknowledge({
      deliveryId: delivery.deliveryId,
      manifestId: applied.manifestId,
      applied,
    });
    return {...acknowledged, applied};
  } catch (error) {
    try {
      await recordFailure({
        deliveryId: delivery.deliveryId,
        message: String(error?.message || error),
      });
    } catch (recordError) {
      console.error('INKBRIDGE_FOLDER_FAILURE_RECORD_ERROR', recordError);
    }
    throw error;
  }
}

export function describeFolderResult(result) {
  const status = result?.status || 'error';
  if (status === 'conflict') {
    return 'Conflict: InkBridge preserved edits from both devices. Automatic sync is paused.';
  }
  if (status === 'error') {
    return `InkBridge error: ${result?.message || 'The folder operation failed.'}`;
  }
  if (result?.applied) {
    const counts = result.applied;
    return `Synced ${counts.operationCount} change(s): ${counts.added} added, ${counts.updated} updated, ${counts.deleted} deleted.`;
  }
  const representedPageIndices = Array.isArray(result?.representedPageIndices)
    ? result.representedPageIndices.filter(page => Number.isInteger(page) && page >= 0)
    : [];
  if (representedPageIndices.length > 1) {
    const pageNumbers = representedPageIndices.map(page => page + 1).join(', ');
    return `Pending sync: pages ${pageNumbers} were finalized together with ${result.strokeCount ?? 0} stroke(s).`;
  }
  if (result?.representedPageCount > 1) {
    return `Pending sync: ${result.representedPageCount} pages were finalized together with ${result.strokeCount ?? 0} stroke(s).`;
  }
  if (result?.pageIndex !== undefined) {
    return `Pending sync: page ${result.pageIndex + 1} was finalized with ${result.strokeCount ?? 0} stroke(s).`;
  }
  if (status === 'pending') {
    if (result?.message) return `Pending sync: ${result.message}`;
    return `Pending sync: ${result.pendingCount ?? 1} incoming update(s) remain.`;
  }
  return 'InkBridge is synced. No incoming changes are waiting.';
}
