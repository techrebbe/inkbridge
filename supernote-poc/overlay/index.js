import {AppRegistry, Image} from 'react-native';
import App, {duplicateFirstStroke, importBooxNativeStroke} from './App';
import {name as appName} from './app.json';
import {PluginManager} from 'sn-plugin-lib';

const DUPLICATE_BUTTON_ID = 100;
const IMPORT_BOOX_BUTTON_ID = 101;
const icon = Image.resolveAssetSource(require('./assets/icon.png')).uri;

AppRegistry.registerComponent(appName, () => App);
PluginManager.init();

PluginManager.registerButton(1, ['NOTE', 'DOC'], {
  id: DUPLICATE_BUTTON_ID,
  name: 'InkBridge Test',
  icon,
  showType: 0,
});

PluginManager.registerButton(1, ['NOTE', 'DOC'], {
  id: IMPORT_BOOX_BUTTON_ID,
  name: 'Import BOOX Test',
  icon,
  // Headless action: import the BOOX-originated stroke and stay in the document.
  showType: 0,
});

PluginManager.registerButtonListener({
  onButtonPress: event => {
    if (event?.id === DUPLICATE_BUTTON_ID) {
      duplicateFirstStroke()
        .then(result => {
          console.log(
            `InkBridge inserted native duplicate on page ${result.page + 1}; source=${result.sourceUuid}`,
          );
        })
        .catch(error => {
          console.error('InkBridge native-stroke proof failed', error);
        });
      return;
    }

    if (event?.id === IMPORT_BOOX_BUTTON_ID) {
      importBooxNativeStroke()
        .then(result => {
          console.log(
            `InkBridge imported BOOX stroke on page ${result.page + 1}; source=${result.sourceUuid}; samples=${result.sampleCount}`,
          );
        })
        .catch(error => {
          console.error('InkBridge BOOX-to-Supernote proof failed', error);
        });
    }
  },
});
