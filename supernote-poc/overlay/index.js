import {AppRegistry, Image} from 'react-native';
import App from './App';
import {name as appName} from './app.json';
import {PluginManager} from 'sn-plugin-lib';
import {
  applyNextFolderManifest,
  getFolderStatus,
  publishCurrentPageExport,
  showFolderResult,
} from './folderCompanion';
import {applyEmbeddedManifest} from './manifestApply';
import {EMBEDDED_MANIFEST} from './generatedManifest';

const EXPORT_SUPERNOTE_BUTTON_ID = 102;
const APPLY_INKBRIDGE_SYNC_BUTTON_ID = 104;
const INKBRIDGE_STATUS_BUTTON_ID = 105;
const APPLY_EMBEDDED_MANIFEST_BUTTON_ID = 106;
const icon = Image.resolveAssetSource(require('./assets/icon.png')).uri;
let folderActionRunning = false;

AppRegistry.registerComponent(appName, () => App);
PluginManager.init();

PluginManager.registerButton(1, ['NOTE', 'DOC'], {
  id: EXPORT_SUPERNOTE_BUTTON_ID,
  name: 'Export InkBridge',
  icon,
  showType: 0,
});

PluginManager.registerButton(1, ['NOTE', 'DOC'], {
  id: APPLY_INKBRIDGE_SYNC_BUTTON_ID,
  name: 'Apply InkBridge Sync',
  icon,
  showType: 0,
});

if (EMBEDDED_MANIFEST) {
  PluginManager.registerButton(1, ['NOTE', 'DOC'], {
    id: APPLY_EMBEDDED_MANIFEST_BUTTON_ID,
    name: 'Apply Embedded Test',
    icon,
    showType: 0,
  });
}

PluginManager.registerButton(1, ['NOTE', 'DOC'], {
  id: INKBRIDGE_STATUS_BUTTON_ID,
  name: 'InkBridge Status',
  icon,
  showType: 0,
});

async function runFolderAction(label, action) {
  if (folderActionRunning) {
    await showFolderResult({
      status: 'pending',
      pendingCount: 1,
      message: 'Another InkBridge action is still running.',
    });
    return;
  }
  folderActionRunning = true;
  try {
    const result = await action();
    await showFolderResult(result);
    console.log(`INKBRIDGE_FOLDER_DONE action=${label} status=${result.status}`);
  } catch (error) {
    console.error(`INKBRIDGE_FOLDER_ERROR action=${label}`, error);
    await showFolderResult({
      status: 'error',
      message: String(error?.message || error),
    });
  } finally {
    folderActionRunning = false;
  }
}

PluginManager.registerButtonListener({
  onButtonPress: event => {
    if (event?.id === EXPORT_SUPERNOTE_BUTTON_ID) {
      runFolderAction('export', publishCurrentPageExport);
      return;
    }

    if (event?.id === APPLY_INKBRIDGE_SYNC_BUTTON_ID) {
      runFolderAction('apply', applyNextFolderManifest);
      return;
    }

    if (event?.id === INKBRIDGE_STATUS_BUTTON_ID) {
      runFolderAction('status', getFolderStatus);
      return;
    }

    if (EMBEDDED_MANIFEST && event?.id === APPLY_EMBEDDED_MANIFEST_BUTTON_ID) {
      runFolderAction('embedded-test', async () => ({
        status: 'synced',
        applied: await applyEmbeddedManifest(),
      }));
    }
  },
});
