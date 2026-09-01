import {
  descriptorMatches,
  parseUserData,
  strokeDescriptor,
} from './manifestCore.js';

const LEDGER_SCHEMA_VERSION = 1;
const SHAPE_TOLERANCE = 0.003;
const INKBRIDGE_ORIGINS = new Set([
  'inkbridge-sync',
  'inkbridge-supernote-native',
]);

function requireObject(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`${label} must be an object.`);
  }
  return value;
}

function requireIdentity(value, label) {
  if (typeof value !== 'string' || !value.trim()) {
    throw new Error(`${label} must be a nonempty string.`);
  }
  return value;
}

function requirePageIndex(value, label) {
  if (!Number.isInteger(value) || value < 0) {
    throw new Error(`${label} must be a nonnegative integer.`);
  }
  return value;
}

function snapshotForStroke(pageIndex, stroke, label) {
  requireObject(stroke, label);
  if (!Array.isArray(stroke.samples) || stroke.samples.length < 2) {
    throw new Error(`${label} must contain at least two samples.`);
  }
  const nativeStyle = {
    layerNum: stroke.layerNum ?? 0,
    thickness: stroke.thickness,
    penColor: stroke.penColor,
    penType: stroke.penType,
  };
  for (const [index, sample] of stroke.samples.entries()) {
    if (
      !Array.isArray(sample) ||
      sample.length !== 3 ||
      !sample.every(Number.isFinite)
    ) {
      throw new Error(`${label} sample ${index} is invalid.`);
    }
  }
  return {
    pageIndex: requirePageIndex(pageIndex, `${label} pageIndex`),
    nativeStyle,
    samples: stroke.samples.map(sample => [...sample]),
  };
}

function payloadPages(payload, label) {
  requireObject(payload, label);
  if (Array.isArray(payload.pages)) {
    return payload.pages.map((page, index) => {
      requireObject(page, `${label} page ${index}`);
      if (!Array.isArray(page.strokes)) {
        throw new Error(`${label} page ${index} has no strokes array.`);
      }
      return {
        pageIndex: requirePageIndex(
          page.pageIndex,
          `${label} page ${index} index`,
        ),
        strokes: page.strokes,
      };
    });
  }
  if (Number.isInteger(payload.pageIndex) && Array.isArray(payload.strokes)) {
    return [{
      pageIndex: requirePageIndex(payload.pageIndex, `${label} pageIndex`),
      strokes: payload.strokes,
    }];
  }
  throw new Error(`${label} has no page snapshots.`);
}

function entryFromStroke(pageIndex, stroke, stableUuid = null) {
  const snapshot = snapshotForStroke(pageIndex, stroke, 'Identity stroke');
  return {
    stableUuid: requireIdentity(
      stableUuid ?? stroke.sourceUuid ?? stroke.sourceKey,
      'Stable stroke identity',
    ),
    nativeUuid: requireIdentity(
      stroke.nativeElementUuid ?? stroke.sourceUuid ?? stroke.sourceKey,
      'Native stroke identity',
    ),
    pageIndex,
    nativeStyle: snapshot.nativeStyle,
    samples: snapshot.samples,
  };
}

function validateLedger(ledger, documentId) {
  if (ledger == null) return [];
  requireObject(ledger, 'Identity ledger');
  if (
    ledger.schemaVersion !== LEDGER_SCHEMA_VERSION ||
    ledger.documentId !== documentId ||
    !Array.isArray(ledger.entries)
  ) {
    throw new Error('Identity ledger authority does not match the open document.');
  }
  return ledger.entries.map((entry, index) => {
    requireObject(entry, `Identity ledger entry ${index}`);
    const snapshot = snapshotForStroke(
      entry.pageIndex,
      {
        ...entry.nativeStyle,
        samples: entry.samples,
      },
      `Identity ledger entry ${index}`,
    );
    return {
      stableUuid: requireIdentity(
        entry.stableUuid,
        `Identity ledger entry ${index} stableUuid`,
      ),
      nativeUuid: requireIdentity(
        entry.nativeUuid,
        `Identity ledger entry ${index} nativeUuid`,
      ),
      ...snapshot,
    };
  });
}

function historyEntries(state, documentId) {
  requireObject(state, 'Identity state');
  if (
    state.schemaVersion !== LEDGER_SCHEMA_VERSION ||
    state.documentId !== documentId ||
    !Array.isArray(state.bootstrapExports)
  ) {
    throw new Error('Identity state authority does not match the open document.');
  }
  const byStableUuid = new Map();
  for (const entry of validateLedger(state.ledger, documentId)) {
    byStableUuid.set(entry.stableUuid, entry);
  }
  for (const [exportIndex, payload] of state.bootstrapExports.entries()) {
    if (payload.documentId && payload.documentId !== documentId) {
      throw new Error(`Identity bootstrap export ${exportIndex} targets another document.`);
    }
    for (const page of payloadPages(payload, `Identity bootstrap export ${exportIndex}`)) {
      for (const stroke of page.strokes) {
        const entry = entryFromStroke(page.pageIndex, stroke);
        const ledgerEntry = byStableUuid.get(entry.stableUuid);
        byStableUuid.set(entry.stableUuid, {
          ...entry,
          nativeUuid: ledgerEntry?.nativeUuid ?? entry.nativeUuid,
        });
      }
    }
  }
  return [...byStableUuid.values()];
}

function sameTranslationIdentityStyle(left, right) {
  return (
    (left.layerNum ?? 0) === (right.layerNum ?? 0) &&
    left.penColor === right.penColor &&
    left.penType === right.penType
  );
}

function translatedShapeMatches(left, right, tolerance = SHAPE_TOLERANCE) {
  // The Supernote portrait/focused lasso path can rewrite native thickness
  // while leaving the complete path, pressure, tool, color, and layer intact.
  // Thickness therefore cannot participate in the translation fallback. If
  // two otherwise-equivalent historical strokes exist, uniqueMatch still
  // rejects the ambiguity instead of guessing an identity.
  if (!sameTranslationIdentityStyle(left.nativeStyle, right.nativeStyle)) {
    return false;
  }
  if (left.samples.length !== right.samples.length) return false;
  const [leftOriginX, leftOriginY] = left.samples[0];
  const [rightOriginX, rightOriginY] = right.samples[0];
  return left.samples.every(([x, y, pressure], index) => {
    const [otherX, otherY, otherPressure] = right.samples[index];
    return (
      Math.abs((x - leftOriginX) - (otherX - rightOriginX)) <= tolerance &&
      Math.abs((y - leftOriginY) - (otherY - rightOriginY)) <= tolerance &&
      Math.abs(pressure - otherPressure) <= 1
    );
  });
}

function uniqueMatch(candidates, label) {
  if (candidates.length > 1) {
    throw new Error(`Stable identity reconciliation is ambiguous for ${label}.`);
  }
  return candidates[0] ?? null;
}

function assignUniqueHistoryMatches(records, history, used, matcher) {
  const claims = [];
  for (const record of records) {
    if (record.stableUuid) continue;
    const candidate = uniqueMatch(
      history.filter(
        entry => !used.has(entry.stableUuid) && matcher(entry, record.current),
      ),
      record.label,
    );
    if (candidate) claims.push({record, candidate});
  }

  const claimantsByStableUuid = new Map();
  for (const claim of claims) {
    const claimants = claimantsByStableUuid.get(claim.candidate.stableUuid) ?? [];
    claimants.push(claim.record.label);
    claimantsByStableUuid.set(claim.candidate.stableUuid, claimants);
  }
  for (const [stableUuid, claimants] of claimantsByStableUuid) {
    if (claimants.length > 1) {
      throw new Error(
        `Stable identity reconciliation is ambiguous for ${stableUuid}: ${claimants.join(', ')}.`,
      );
    }
  }
  for (const {record, candidate} of claims) {
    record.stableUuid = candidate.stableUuid;
    used.add(candidate.stableUuid);
  }
}

function retainedIdentity(stroke) {
  const tagged = parseUserData(stroke.userData);
  if (!INKBRIDGE_ORIGINS.has(tagged?.inkBridgeOrigin)) return null;
  const retained = requireIdentity(tagged.sourceUuid, 'Retained stroke identity');
  if (stroke.sourceUuid !== retained) {
    throw new Error('Retained stroke identity disagrees with the exported stroke.');
  }
  return retained;
}

function rewriteIdentity(stroke, stableUuid) {
  const rewritten = {...stroke, sourceUuid: stableUuid, sourceKey: stableUuid};
  delete rewritten.nativeElementUuid;
  return rewritten;
}

export function reconcileStableStrokeIdentities(documentId, payload, state) {
  requireIdentity(documentId, 'Document identity');
  const history = historyEntries(state, documentId);
  const pages = payloadPages(payload, 'Current export');
  const used = new Set();
  const representedPages = new Set(pages.map(page => page.pageIndex));
  const records = [];
  for (const page of pages) {
    for (const [strokeIndex, stroke] of page.strokes.entries()) {
      const label = `page ${page.pageIndex + 1} stroke ${strokeIndex + 1}`;
      const current = entryFromStroke(page.pageIndex, stroke);
      const stableUuid = retainedIdentity(stroke);
      if (stableUuid) {
        if (used.has(stableUuid)) {
          throw new Error(`Stable identity ${stableUuid} appears more than once in this export.`);
        }
        used.add(stableUuid);
      }
      records.push({pageIndex: page.pageIndex, stroke, current, label, stableUuid});
    }
  }

  // A retained InkBridge tag or an unchanged native UUID is authoritative.
  // Resolve those first, then inspect every remaining current stroke together
  // so two copies cannot greedily claim the same historical identity.
  assignUniqueHistoryMatches(
    records,
    history,
    used,
    (entry, current) => entry.nativeUuid === current.nativeUuid,
  );
  assignUniqueHistoryMatches(
    records,
    history,
    used,
    (entry, current) => {
      const exact =
        entry.pageIndex === current.pageIndex &&
        descriptorMatches(strokeDescriptor(entry), strokeDescriptor(current));
      return exact || translatedShapeMatches(entry, current);
    },
  );

  const rewrittenByPage = new Map(pages.map(page => [page.pageIndex, []]));
  const nextEntries = [];
  for (const record of records) {
    if (!record.stableUuid) {
      record.stableUuid = record.current.nativeUuid;
      if (used.has(record.stableUuid)) {
        throw new Error(
          `Stable identity ${record.stableUuid} appears more than once in this export.`,
        );
      }
      used.add(record.stableUuid);
    }
    rewrittenByPage
      .get(record.pageIndex)
      .push(rewriteIdentity(record.stroke, record.stableUuid));
    nextEntries.push({...record.current, stableUuid: record.stableUuid});
  }

  const rewrittenPayload = JSON.parse(JSON.stringify(payload));
  for (const page of payloadPages(rewrittenPayload, 'Rewritten export')) {
    page.strokes.splice(
      0,
      page.strokes.length,
      ...rewrittenByPage.get(page.pageIndex),
    );
  }
  const retainedEntries = history.filter(
    entry => !representedPages.has(entry.pageIndex) && !used.has(entry.stableUuid),
  );
  const entries = [...retainedEntries, ...nextEntries].sort(
    (left, right) =>
      left.pageIndex - right.pageIndex ||
      left.stableUuid.localeCompare(right.stableUuid),
  );
  return {
    payload: rewrittenPayload,
    ledger: {
      schemaVersion: LEDGER_SCHEMA_VERSION,
      documentId,
      entries,
    },
  };
}
