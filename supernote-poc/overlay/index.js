import {AppRegistry, Image} from 'react-native';
import App, {duplicateFirstStroke} from './App';
import {name as appName} from './app.json';
import {PluginManager} from 'sn-plugin-lib';

const BUTTON_ID = 100;

AppRegistry.registerComponent(appName, () => App);
PluginManager.init();

PluginManager.registerButton(1, ['NOTE', 'DOC'], {
  id: BUTTON_ID,
  name: 'InkBridge Test',
  icon: Image.resolveAssetSource(require('./assets/icon.png')).uri,
  // Run the proof as a headless toolbar action. The user remains in the
  // document, so the duplicated stroke can immediately be lassoed/erased.
  showType: 0,
});

PluginManager.registerButtonListener({
  onButtonPress: event => {
    if (event?.id !== BUTTON_ID) return;

    duplicateFirstStroke()
      .then(result => {
        console.log(
          `InkBridge inserted native duplicate on page ${result.page + 1}; source=${result.sourceUuid}`,
        );
      })
      .catch(error => {
        console.error('InkBridge native-stroke proof failed', error);
      });
  },
});
