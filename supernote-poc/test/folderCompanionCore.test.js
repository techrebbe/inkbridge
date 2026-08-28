import assert from 'node:assert/strict';
import test from 'node:test';
import {
  describeFolderResult,
  parseNativeJson,
  processManifestDelivery,
  revalidateCollectedDocument,
  requireCompatibleTargetFileName,
  requirePluginResult,
  requireSameDocumentId,
  requireSameDocumentPath,
} from '../overlay/folderCompanionCore.js';

test('native JSON parsing rejects empty and malformed results', () => {
  assert.throws(() => parseNativeJson('', 'status'), /returned no result/);
  assert.throws(() => parseNativeJson('{', 'status'), /invalid JSON/);
  assert.deepEqual(parseNativeJson('{"status":"synced"}', 'status'), {
    status: 'synced',
  });
});

test('stable folder identity permits a renamed Supernote PDF', () => {
  assert.doesNotThrow(() =>
    requireCompatibleTargetFileName(['Old Name.pdf'], 'Renamed.pdf', true),
  );
  assert.throws(
    () => requireCompatibleTargetFileName(['Old Name.pdf'], 'Renamed.pdf'),
    /targets Old Name.pdf/,
  );
});

test('manifest application remains bound to the document that loaded it', () => {
  assert.equal(
    requireSameDocumentPath('/EXPORT/first/book.pdf', '/EXPORT/first/book.pdf'),
    '/EXPORT/first/book.pdf',
  );
  assert.throws(
    () =>
      requireSameDocumentPath(
        '/EXPORT/first/book.pdf',
        '/EXPORT/second/book.pdf',
      ),
    /open document changed/,
  );
});

test('page export is revalidated against the still-open document', async () => {
  assert.equal(
    await revalidateCollectedDocument('/EXPORT/first.pdf', async () => '/EXPORT/first.pdf'),
    '/EXPORT/first.pdf',
  );
  await assert.rejects(
    revalidateCollectedDocument('/EXPORT/first.pdf', async () => '/EXPORT/second.pdf'),
    /open document changed/,
  );
});

test('manifest application remains bound to freshly hashed PDF content', async () => {
  assert.equal(requireSameDocumentId('doc-a', 'doc-a'), 'doc-a');
  assert.throws(() => requireSameDocumentId('doc-a', 'doc-b'), /PDF content changed/);

  const order = [];
  await processManifestDelivery({
    delivery: {deliveryId: 'abc', documentId: 'doc-a', manifest: {manifestId: 'm1'}},
    validate: async delivery => order.push(`validate:${delivery.documentId}`),
    apply: async manifest => {
      order.push(`apply:${manifest.manifestId}`);
      return {manifestId: manifest.manifestId};
    },
    acknowledge: async () => ({status: 'synced'}),
    recordFailure: async () => assert.fail('nothing failed'),
  });
  assert.deepEqual(order, ['validate:doc-a', 'apply:m1']);
});

test('official plugin responses use the result field', () => {
  assert.equal(
    requirePluginResult({success: true, result: '/EXPORT/book.pdf'}, 'path'),
    '/EXPORT/book.pdf',
  );
  assert.throws(
    () => requirePluginResult({success: true, data: '/wrong-field'}, 'path'),
    /returned no result/,
  );
  assert.throws(
    () => requirePluginResult({success: false, error: {message: 'denied'}}, 'path'),
    /denied/,
  );
});

test('manifest delivery is applied before its durable acknowledgement', async () => {
  const order = [];
  const result = await processManifestDelivery({
    delivery: {deliveryId: 'abc', manifest: {manifestId: 'm1'}},
    apply: async manifest => {
      order.push(`apply:${manifest.manifestId}`);
      return {manifestId: manifest.manifestId, operationCount: 1, added: 1};
    },
    acknowledge: async ({deliveryId, manifestId}) => {
      order.push(`ack:${deliveryId}:${manifestId}`);
      return {status: 'synced', pendingCount: 0};
    },
    recordFailure: async () => assert.fail('failure must not be recorded'),
  });
  assert.deepEqual(order, ['apply:m1', 'ack:abc:m1']);
  assert.equal(result.status, 'synced');
  assert.equal(result.applied.added, 1);
});

test('failed application remains unacknowledged and records a retryable error', async () => {
  const calls = [];
  await assert.rejects(
    processManifestDelivery({
      delivery: {deliveryId: 'abc', manifest: {manifestId: 'm1'}},
      apply: async () => {
        calls.push('apply');
        throw new Error('native write failed');
      },
      acknowledge: async () => calls.push('ack'),
      recordFailure: async failure => calls.push(`error:${failure.deliveryId}`),
    }),
    /native write failed/,
  );
  assert.deepEqual(calls, ['apply', 'error:abc']);
});

test('duplicate-free no-delivery response performs no write', async () => {
  const result = await processManifestDelivery({
    delivery: {status: 'synced', pendingCount: 0},
    apply: async () => assert.fail('no manifest should be applied'),
    acknowledge: async () => assert.fail('nothing should be acknowledged'),
    recordFailure: async () => assert.fail('nothing failed'),
  });
  assert.deepEqual(result, {status: 'synced', pendingCount: 0});
});

test('status descriptions distinguish pending, conflict, error, and synced', () => {
  assert.match(describeFolderResult({status: 'pending', pendingCount: 2}), /2 incoming/);
  assert.match(
    describeFolderResult({status: 'pending', message: 'another action is running'}),
    /another action is running/,
  );
  assert.match(describeFolderResult({status: 'conflict'}), /preserved edits from both/);
  assert.match(describeFolderResult({status: 'error', message: 'bad file'}), /bad file/);
  assert.match(describeFolderResult({status: 'synced'}), /is synced/);
});

test('spread export status reports every represented source page', () => {
  assert.equal(
    describeFolderResult({
      status: 'pending',
      pageIndex: 1,
      representedPageCount: 2,
      representedPageIndices: [1, 2],
      strokeCount: 3,
    }),
    'Pending sync: pages 2, 3 were finalized together with 3 stroke(s).',
  );
  assert.equal(
    describeFolderResult({
      status: 'pending',
      pageIndex: 1,
      representedPageCount: 2,
      strokeCount: 3,
    }),
    'Pending sync: 2 pages were finalized together with 3 stroke(s).',
  );
});
