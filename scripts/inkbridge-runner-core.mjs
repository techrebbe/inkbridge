import path from 'node:path';

export function parseAdbDevices(output) {
  return output
    .split(/\r?\n/u)
    .map(line => line.trim())
    .filter(line => line && !line.startsWith('List of devices'))
    .map(line => {
      const [serial, state, ...details] = line.split(/\s+/u);
      const metadata = {};
      for (const detail of details) {
        const separator = detail.indexOf(':');
        if (separator > 0) {
          metadata[detail.slice(0, separator)] = detail.slice(separator + 1);
        }
      }
      return {serial, state, ...metadata};
    });
}

export function classifyDevice(device) {
  const identity = [
    device.serial,
    device.manufacturer,
    device.model,
    device.product,
    device.device,
  ]
    .filter(Boolean)
    .join(' ')
    .toLowerCase();
  if (/onyx|boox|noteair/u.test(identity)) return 'boox';
  if (/supernote|ratta/u.test(identity)) return 'supernote';
  return null;
}

export function selectDevice(devices, role, requestedSerial) {
  const online = devices.filter(device => device.state === 'device');
  if (requestedSerial) {
    const requested = online.find(device => device.serial === requestedSerial);
    if (!requested) {
      throw new Error(`${role} device ${requestedSerial} is not connected and authorized.`);
    }
    const detectedRole = classifyDevice(requested);
    if (detectedRole && detectedRole !== role) {
      throw new Error(
        `${requestedSerial} identifies as ${detectedRole}, not the requested ${role} device.`,
      );
    }
    return requested;
  }

  const candidates = online.filter(device => classifyDevice(device) === role);
  if (candidates.length === 0) {
    throw new Error(`No connected ${role} device was detected.`);
  }
  if (candidates.length > 1) {
    throw new Error(
      `Multiple ${role} devices are connected; pass --${role} <serial>.`,
    );
  }
  return candidates[0];
}

export function parseExportProgress(output) {
  let expectedTotal = null;
  const chunks = new Map();
  let doneLine = null;
  for (const rawLine of output.split(/\r?\n/u)) {
    const doneAt = rawLine.indexOf('INKBRIDGE_EXPORT_DONE');
    if (doneAt >= 0) {
      doneLine = rawLine.slice(doneAt).trim();
    }

    const markerAt = rawLine.indexOf('INKBRIDGE_EXPORT ');
    if (markerAt < 0) continue;
    const marked = rawLine.slice(markerAt).trim();
    const match = /^INKBRIDGE_EXPORT\s+(\d+)\/(\d+)\s+(.+)$/u.exec(marked);
    if (!match) continue;
    const index = Number(match[1]);
    const total = Number(match[2]);
    if (!Number.isSafeInteger(index) || !Number.isSafeInteger(total) || index < 1) {
      throw new Error('The Supernote export contains an invalid chunk number.');
    }
    if (expectedTotal !== null && expectedTotal !== total) {
      throw new Error('The Supernote export contains inconsistent chunk totals.');
    }
    expectedTotal = total;
    chunks.set(index, match[3]);
  }

  const found = chunks.size;
  const complete =
    expectedTotal !== null &&
    found === expectedTotal &&
    Array.from({length: expectedTotal}, (_, index) => index + 1).every(index =>
      chunks.has(index),
    ) &&
    doneLine !== null;
  const lines = expectedTotal
    ? Array.from({length: expectedTotal}, (_, index) => {
        const sequence = index + 1;
        return chunks.has(sequence)
          ? `INKBRIDGE_EXPORT ${sequence}/${expectedTotal} ${chunks.get(sequence)}`
          : null;
      }).filter(Boolean)
    : [];
  if (doneLine) lines.push(doneLine);
  return {complete, expectedTotal, found, lines};
}

export function versionedPluginConfig(baseConfig, now = new Date()) {
  const epochSeconds = Math.floor(now.getTime() / 1000);
  const baseVersion = Number.parseInt(baseConfig.versionCode, 10);
  const versionCode = Math.max(
    Number.isSafeInteger(baseVersion) ? baseVersion + 1 : 1,
    epochSeconds,
  );
  if (versionCode > 2_147_483_647) {
    throw new Error('Generated plugin version exceeds the Supernote version-code limit.');
  }
  const stamp = now.toISOString().replace(/[-:]/gu, '').replace(/\.\d{3}Z$/u, 'Z');
  return {
    ...baseConfig,
    versionCode: String(versionCode),
    versionName: `sync-${stamp}`,
  };
}

export function validateDevicePath(value, label) {
  if (typeof value !== 'string' || !value.startsWith('/')) {
    throw new Error(`${label} must be an absolute Android storage path.`);
  }
  if (/[\0\r\n]/u.test(value) || value.split('/').includes('..')) {
    throw new Error(`${label} contains an unsafe path segment.`);
  }
  if (!value.startsWith('/sdcard/') && !value.startsWith('/storage/emulated/0/')) {
    throw new Error(`${label} must stay within shared Android storage.`);
  }
  return value;
}

export function shellQuote(value) {
  return `'${value.replaceAll("'", "'\\''")}'`;
}

export function toBashPath(value, platform = process.platform) {
  if (platform !== 'win32') return value;
  const normalized = path.win32.resolve(value).replaceAll('\\', '/');
  const match = /^([A-Za-z]):\/(.*)$/u.exec(normalized);
  return match ? `/${match[1].toLowerCase()}/${match[2]}` : normalized;
}

export function chooseCargoToolchain(platform, installedToolchains, explicit) {
  if (explicit) return explicit;
  if (
    platform === 'win32' &&
    installedToolchains.some(toolchain =>
      toolchain.startsWith('stable-x86_64-pc-windows-gnu'),
    )
  ) {
    return 'stable-x86_64-pc-windows-gnu';
  }
  return null;
}
