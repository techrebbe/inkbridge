import assert from 'node:assert/strict';
import test from 'node:test';
import {
  chooseCargoToolchain,
  classifyDevice,
  parseAdbDevices,
  parseExportProgress,
  selectDevice,
  shellQuote,
  toBashPath,
  validateDevicePath,
  versionedPluginConfig,
} from '../inkbridge-runner-core.mjs';

const devicesOutput = `List of devices attached
117b3062 device product:NoteAir4C model:NoteAir4C device:NoteAir4C transport_id:2
SN078C10015092 device product:Supernote model:Supernote_Nomad device:Supernote transport_id:4
offline-device offline transport_id:5
`;

test('ADB device inventory identifies the connected BOOX and Supernote', () => {
  const devices = parseAdbDevices(devicesOutput).map(device =>
    device.serial === '117b3062'
      ? {...device, manufacturer: 'ONYX'}
      : {...device, manufacturer: 'Supernote'},
  );
  assert.equal(classifyDevice(devices[0]), 'boox');
  assert.equal(classifyDevice(devices[1]), 'supernote');
  assert.equal(selectDevice(devices, 'boox').serial, '117b3062');
  assert.equal(selectDevice(devices, 'supernote').serial, 'SN078C10015092');
});

test('explicit serial cannot silently select the wrong device type', () => {
  const devices = parseAdbDevices(devicesOutput).map(device =>
    device.serial === '117b3062'
      ? {...device, manufacturer: 'ONYX'}
      : {...device, manufacturer: 'Supernote'},
  );
  assert.throws(
    () => selectDevice(devices, 'supernote', '117b3062'),
    /identifies as boox/u,
  );
});

test('export capture requires every numbered chunk and the completion marker', () => {
  const partial = parseExportProgress(
    'noise\nINKBRIDGE_EXPORT 1/2 {"pageIndex":0,\n',
  );
  assert.equal(partial.complete, false);
  assert.equal(partial.found, 1);

  const complete = parseExportProgress(
    'INKBRIDGE_EXPORT 1/2 {"pageIndex":0,\n' +
      'INKBRIDGE_EXPORT 2/2 "strokes":[]}\n' +
      'INKBRIDGE_EXPORT_DONE page=1 strokes=0\n',
  );
  assert.equal(complete.complete, true);
  assert.deepEqual(complete.lines, [
    'INKBRIDGE_EXPORT 1/2 {"pageIndex":0,',
    'INKBRIDGE_EXPORT 2/2 "strokes":[]}',
    'INKBRIDGE_EXPORT_DONE page=1 strokes=0',
  ]);
});

test('plugin build gets a monotonic Android-safe version code', () => {
  const config = versionedPluginConfig(
    {versionCode: '16', versionName: '0.1.4'},
    new Date('2026-08-11T12:34:56Z'),
  );
  assert.equal(config.versionCode, '1786451696');
  assert.equal(config.versionName, 'sync-20260811T123456Z');
});

test('device paths stay within shared storage and reject traversal', () => {
  assert.equal(
    validateDevicePath('/sdcard/Books/My Document.pdf', '--boox-pdf'),
    '/sdcard/Books/My Document.pdf',
  );
  assert.throws(
    () => validateDevicePath('/sdcard/Books/../private/file.pdf', '--boox-pdf'),
    /unsafe path segment/u,
  );
  assert.throws(
    () => validateDevicePath('/data/local/file.pdf', '--boox-pdf'),
    /shared Android storage/u,
  );
});

test('shell and Git Bash paths are escaped deterministically', () => {
  assert.equal(shellQuote("/sdcard/MyStyle/reader's file.snplg"), "'/sdcard/MyStyle/reader'\\''s file.snplg'");
  assert.equal(toBashPath('C:\\Work\\InkBridge\\manifest.json', 'win32'), '/c/Work/InkBridge/manifest.json');
});

test('Windows prefers an installed GNU Rust toolchain when no override is given', () => {
  assert.equal(
    chooseCargoToolchain(
      'win32',
      ['1.90.0-x86_64-pc-windows-msvc', 'stable-x86_64-pc-windows-gnu'],
      null,
    ),
    'stable-x86_64-pc-windows-gnu',
  );
  assert.equal(
    chooseCargoToolchain('win32', ['stable-x86_64-pc-windows-gnu'], 'nightly'),
    'nightly',
  );
  assert.equal(chooseCargoToolchain('linux', ['stable-x86_64-pc-windows-gnu']), null);
});
