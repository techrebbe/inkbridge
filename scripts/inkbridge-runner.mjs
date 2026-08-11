#!/usr/bin/env node

import {spawnSync} from 'node:child_process';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
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
} from './inkbridge-runner-core.mjs';

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(scriptDirectory, '..');

function fail(message) {
  throw new Error(message);
}

function parseArguments(argv) {
  const args = [...argv];
  const command = args.shift();
  const values = new Map();
  const flags = new Set();
  const repeated = new Map();
  const repeatable = new Set(['--baseline']);
  const booleanFlags = new Set(['--no-push']);
  while (args.length > 0) {
    const name = args.shift();
    if (!name?.startsWith('--')) fail(`Unexpected argument: ${name ?? ''}`);
    if (booleanFlags.has(name)) {
      flags.add(name);
      continue;
    }
    const value = args.shift();
    if (!value || value.startsWith('--')) fail(`${name} requires a value.`);
    if (repeatable.has(name)) {
      const existing = repeated.get(name) ?? [];
      existing.push(value);
      repeated.set(name, existing);
    } else {
      if (values.has(name)) fail(`${name} may only be supplied once.`);
      values.set(name, value);
    }
  }
  return {command, values, flags, repeated};
}

function option(parsed, name, fallback = null) {
  return parsed.values.get(name) ?? fallback;
}

function requireOption(parsed, name) {
  const value = option(parsed, name);
  if (!value) fail(`${name} is required.`);
  return value;
}

function run(command, args, {capture = false, env = process.env} = {}) {
  const result = spawnSync(command, args, {
    cwd: repositoryRoot,
    encoding: 'utf8',
    env,
    stdio: capture ? ['ignore', 'pipe', 'pipe'] : 'inherit',
    windowsHide: true,
  });
  if (result.error) {
    fail(`Could not run ${command}: ${result.error.message}`);
  }
  if (result.status !== 0) {
    const detail = capture ? (result.stderr || result.stdout || '').trim() : '';
    fail(`${command} exited with code ${result.status}${detail ? `: ${detail}` : ''}`);
  }
  return capture ? result.stdout : '';
}

function findExecutable(explicit, environmentName, candidates, fallback) {
  if (explicit) {
    if (!fs.existsSync(explicit)) fail(`${explicit} does not exist.`);
    return path.resolve(explicit);
  }
  const environmentValue = process.env[environmentName];
  if (environmentValue) {
    if (!fs.existsSync(environmentValue)) {
      fail(`${environmentName} points to a missing file: ${environmentValue}`);
    }
    return path.resolve(environmentValue);
  }
  const existing = candidates.find(candidate => candidate && fs.existsSync(candidate));
  return existing ?? fallback;
}

function findAdb(parsed) {
  const candidates = process.platform === 'win32'
    ? [
        path.join(
          process.env.LOCALAPPDATA ?? '',
          'Android',
          'Sdk',
          'platform-tools',
          'adb.exe',
        ),
        path.join(
          os.homedir(),
          'Downloads',
          'platform-tools-latest-windows',
          'platform-tools',
          'adb.exe',
        ),
      ]
    : [];
  return findExecutable(option(parsed, '--adb'), 'INKBRIDGE_ADB', candidates, 'adb');
}

function findBash(parsed) {
  const candidates = process.platform === 'win32'
    ? [
        'C:\\Program Files\\Git\\bin\\bash.exe',
        'C:\\Program Files\\Git\\usr\\bin\\bash.exe',
      ]
    : ['/bin/bash', '/usr/bin/bash'];
  return findExecutable(option(parsed, '--bash'), 'INKBRIDGE_BASH', candidates, 'bash');
}

function adbOutput(adb, ...args) {
  return run(adb, args, {capture: true}).trim();
}

function probeDevices(adb) {
  const devices = parseAdbDevices(adbOutput(adb, 'devices', '-l'));
  return devices.map(device => {
    if (device.state !== 'device') return device;
    const manufacturer = adbOutput(
      adb,
      '-s',
      device.serial,
      'shell',
      'getprop',
      'ro.product.manufacturer',
    );
    const model = adbOutput(
      adb,
      '-s',
      device.serial,
      'shell',
      'getprop',
      'ro.product.model',
    );
    return {...device, manufacturer, model};
  });
}

function resolveDevices(parsed, {needBoox = true, needSupernote = true} = {}) {
  const adb = findAdb(parsed);
  run(adb, ['version'], {capture: true});
  const devices = probeDevices(adb);
  const boox = needBoox
    ? selectDevice(devices, 'boox', option(parsed, '--boox'))
    : null;
  const supernote = needSupernote
    ? selectDevice(devices, 'supernote', option(parsed, '--supernote'))
    : null;
  return {adb, devices, boox, supernote};
}

function printDoctor(parsed) {
  const {adb, devices, boox, supernote} = resolveDevices(parsed);
  console.log(`ADB: ${adb}`);
  for (const device of devices) {
    console.log(
      `${device.serial}: ${device.state} | ${device.manufacturer ?? 'unknown'} ${device.model ?? ''} | ${classifyDevice(device) ?? 'unclassified'}`,
    );
  }
  console.log(`BOOX: ${boox.serial} (${boox.model})`);
  console.log(`Supernote: ${supernote.serial} (${supernote.model})`);
}

const sleep = milliseconds => new Promise(resolve => setTimeout(resolve, milliseconds));

async function captureBaseline(parsed) {
  const output = path.resolve(requireOption(parsed, '--output'));
  const timeoutSeconds = Number(option(parsed, '--timeout-seconds', '180'));
  if (!Number.isFinite(timeoutSeconds) || timeoutSeconds < 10) {
    fail('--timeout-seconds must be at least 10.');
  }
  const {adb, supernote} = resolveDevices(parsed, {needBoox: false});
  fs.mkdirSync(path.dirname(output), {recursive: true});
  run(adb, ['-s', supernote.serial, 'logcat', '-c'], {capture: true});
  console.log(`Supernote ${supernote.serial} is ready.`);
  console.log('Open the target page and tap Export Page Test. Waiting for completion...');

  const deadline = Date.now() + timeoutSeconds * 1000;
  let lastFound = -1;
  while (Date.now() < deadline) {
    const log = adbOutput(adb, '-s', supernote.serial, 'logcat', '-d', '-v', 'raw');
    const progress = parseExportProgress(log);
    if (progress.found !== lastFound) {
      lastFound = progress.found;
      if (progress.expectedTotal) {
        console.log(`Captured ${progress.found}/${progress.expectedTotal} chunks.`);
      }
    }
    if (progress.complete) {
      const temporary = `${output}.tmp`;
      fs.writeFileSync(temporary, `${progress.lines.join('\n')}\n`, 'utf8');
      fs.renameSync(temporary, output);
      console.log(`Baseline saved: ${output}`);
      return;
    }
    await sleep(1000);
  }
  fail(`Timed out after ${timeoutSeconds} seconds without a complete export.`);
}

function sha256(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

function newestPlugin(directory) {
  const plugins = fs
    .readdirSync(directory)
    .filter(name => name.endsWith('.snplg'))
    .map(name => path.join(directory, name));
  if (plugins.length !== 1) {
    fail(`Expected exactly one built .snplg in ${directory}, found ${plugins.length}.`);
  }
  return plugins[0];
}

function pushPlugin(parsed, plugin) {
  const resolvedPlugin = path.resolve(plugin);
  if (!fs.existsSync(resolvedPlugin) || !resolvedPlugin.endsWith('.snplg')) {
    fail(`Plugin does not exist or is not an .snplg package: ${resolvedPlugin}`);
  }
  const {adb, supernote} = resolveDevices(parsed, {
    needBoox: false,
    needSupernote: true,
  });
  const remoteDirectory = validateDevicePath(
    option(parsed, '--supernote-dir', '/sdcard/MyStyle'),
    '--supernote-dir',
  );
  const remotePlugin = `${remoteDirectory}/${path.basename(resolvedPlugin)}`;
  run(adb, [
    '-s',
    supernote.serial,
    'shell',
    `mkdir -p ${shellQuote(remoteDirectory)}`,
  ]);
  console.log(`Copying plugin to Supernote ${supernote.serial}...`);
  run(adb, ['-s', supernote.serial, 'push', resolvedPlugin, remotePlugin]);
  const localHash = sha256(resolvedPlugin);
  const remoteHashOutput = adbOutput(
    adb,
    '-s',
    supernote.serial,
    'shell',
    `sha256sum ${shellQuote(remotePlugin)}`,
  );
  const remoteHash = remoteHashOutput.split(/\s+/u)[0]?.toLowerCase();
  if (remoteHash !== localHash) {
    fail(`Supernote copy verification failed: expected ${localHash}, got ${remoteHash}.`);
  }
  console.log(`Verified on Supernote: ${remotePlugin}`);
  return remotePlugin;
}

function cargoToolchain(parsed) {
  const explicit = option(parsed, '--cargo-toolchain') ?? process.env.INKBRIDGE_RUST_TOOLCHAIN;
  if (explicit) return explicit;
  if (process.platform !== 'win32') return null;
  try {
    const installed = run('rustup', ['toolchain', 'list'], {capture: true})
      .split(/\r?\n/u)
      .map(line => line.trim().split(/\s+/u)[0])
      .filter(Boolean);
    return chooseCargoToolchain(process.platform, installed, null);
  } catch {
    return null;
  }
}

function prepareReturn(parsed) {
  const baselines = parsed.repeated.get('--baseline') ?? [];
  if (baselines.length === 0) fail('At least one --baseline is required.');
  const localPdfOption = option(parsed, '--pdf');
  const booxPdfOption = option(parsed, '--boox-pdf');
  if (Boolean(localPdfOption) === Boolean(booxPdfOption)) {
    fail('Supply exactly one of --pdf or --boox-pdf.');
  }
  const shouldPush = !parsed.flags.has('--no-push');
  const needsBoox = Boolean(booxPdfOption);
  const devices = needsBoox
    ? resolveDevices(parsed, {needBoox: true, needSupernote: false})
    : null;
  const stamp = new Date().toISOString().replace(/[-:]/gu, '').replace(/\.\d{3}Z$/u, 'Z');
  const outputDirectory = path.resolve(
    option(parsed, '--output-dir', path.join(repositoryRoot, 'inkbridge-runs', stamp)),
  );
  fs.mkdirSync(outputDirectory, {recursive: true});

  let pdf;
  if (booxPdfOption) {
    const remotePdf = validateDevicePath(booxPdfOption, '--boox-pdf');
    pdf = path.join(outputDirectory, path.basename(remotePdf));
    console.log(`Pulling ${remotePdf} from BOOX ${devices.boox.serial}...`);
    run(devices.adb, ['-s', devices.boox.serial, 'pull', remotePdf, pdf]);
  } else {
    pdf = path.resolve(localPdfOption);
    if (!fs.existsSync(pdf)) fail(`PDF does not exist: ${pdf}`);
  }

  const resolvedBaselines = baselines.map(value => {
    const baseline = path.resolve(value);
    if (!fs.existsSync(baseline)) fail(`Baseline does not exist: ${baseline}`);
    return baseline;
  });
  const manifest = path.join(outputDirectory, 'inkbridge-manifest.json');
  const cargoArgs = [
    'run',
    '--quiet',
    '-p',
    'inkbridge-convert',
    '--',
    'extract',
    '--pdf',
    pdf,
  ];
  const toolchain = cargoToolchain(parsed);
  if (toolchain) cargoArgs.unshift(`+${toolchain}`);
  for (const baseline of resolvedBaselines) {
    cargoArgs.push('--baseline', baseline);
  }
  cargoArgs.push('--output', manifest);
  const yOffset = option(parsed, '--y-offset');
  if (yOffset) cargoArgs.push('--y-offset', yOffset);
  console.log('Creating the InkBridge manifest...');
  run(process.env.INKBRIDGE_CARGO ?? 'cargo', cargoArgs);

  const baseConfig = JSON.parse(
    fs.readFileSync(path.join(repositoryRoot, 'supernote-poc', 'PluginConfig.json'), 'utf8'),
  );
  const pluginConfig = versionedPluginConfig(baseConfig);
  const generatedConfig = path.join(outputDirectory, 'PluginConfig.json');
  fs.writeFileSync(generatedConfig, `${JSON.stringify(pluginConfig, null, 2)}\n`, 'utf8');

  const buildOutput = path.join(outputDirectory, 'plugin');
  fs.mkdirSync(buildOutput, {recursive: true});
  const bash = findBash(parsed);
  const buildScript = path.join(repositoryRoot, 'supernote-poc', 'build.sh');
  console.log(`Building Supernote plugin ${pluginConfig.versionName}...`);
  run(
    bash,
    [toBashPath(buildScript), toBashPath(manifest)],
    {
      env: {
        ...process.env,
        INKBRIDGE_PLUGIN_CONFIG: toBashPath(generatedConfig),
        INKBRIDGE_OUTPUT_DIR: toBashPath(buildOutput),
      },
    },
  );
  const builtPlugin = newestPlugin(buildOutput);
  const deliverable = path.join(outputDirectory, `InkBridge-${pluginConfig.versionName}.snplg`);
  fs.copyFileSync(builtPlugin, deliverable);
  const localHash = sha256(deliverable);
  console.log(`Plugin built: ${deliverable}`);
  console.log(`SHA-256: ${localHash}`);

  if (shouldPush) {
    pushPlugin(parsed, deliverable);
    console.log('Next: update InkBridge in the Supernote plugin manager, open the original PDF,');
    console.log('then tap Apply InkBridge Sync once.');
  }
}

function usage() {
  return `InkBridge Runner

Usage:
  scripts\\InkBridge-Runner.cmd doctor [--adb PATH] [--boox SERIAL] [--supernote SERIAL]
  scripts\\InkBridge-Runner.cmd capture --output PAGE.log [--timeout-seconds 180]
  scripts\\InkBridge-Runner.cmd push --plugin PACKAGE.snplg [--supernote-dir /sdcard/MyStyle]
  scripts\\InkBridge-Runner.cmd prepare (--boox-pdf DEVICE_PATH | --pdf LOCAL_PATH) \\
    --baseline PAGE.log [--baseline PAGE2.log] [--output-dir DIRECTORY] \\
    [--supernote-dir /sdcard/MyStyle] [--no-push]

Device serials are normally detected automatically. Use --boox or --supernote only
when more than one device of the same type is connected. On Windows, an installed
GNU Rust toolchain is selected automatically; override it with --cargo-toolchain.`;
}

async function main() {
  const parsed = parseArguments(process.argv.slice(2));
  switch (parsed.command) {
    case 'doctor':
      printDoctor(parsed);
      break;
    case 'capture':
      await captureBaseline(parsed);
      break;
    case 'prepare':
      prepareReturn(parsed);
      break;
    case 'push':
      pushPlugin(parsed, requireOption(parsed, '--plugin'));
      break;
    case '--help':
    case '-h':
    case 'help':
    case undefined:
      console.log(usage());
      break;
    default:
      fail(`Unknown command: ${parsed.command}\n\n${usage()}`);
  }
}

main().catch(error => {
  console.error(`InkBridge Runner: ${error.message}`);
  process.exitCode = 2;
});
