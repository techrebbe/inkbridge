import {AppRegistry, Image} from 'react-native';
import App, {
  duplicateFirstStroke,
  exportCurrentSupernotePage,
  importBooxNativeStroke,
} from './App';
import {applyBooxReturnTest} from './returnApplyV2';
import {name as appName} from './app.json';
import {PluginManager} from 'sn-plugin-lib';

const DUPLICATE_BUTTON_ID = 100;
const IMPORT_BOOX_BUTTON_ID = 101;
const EXPORT_SUPERNOTE_BUTTON_ID = 102;
const APPLY_BOOX_RETURN_BUTTON_ID = 103;
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
  showType: 0,
});

PluginManager.registerButton(1, ['NOTE', 'DOC'], {
  id: EXPORT_SUPERNOTE_BUTTON_ID,
  name: 'Export Page Test',
  icon,
  showType: 0,
});

PluginManager.registerButton(1, ['NOTE', 'DOC'], {
  id: APPLY_BOOX_RETURN_BUTTON_ID,
  name: 'Apply BOOX Return Test',
  icon,
  showType: 0,
});

PluginManager.registerButtonListener({
  onButtonPress: event => {
    if (event?.id === DUPLICATE_BUTTON_ID) {
      duplicateFirstStroke()
        .then(result => {
          console.log(`InkBridge duplicate page=${result.page + 1} source=${result.sourceUuid}`);
        })
        .catch(error => console.error('InkBridge duplicate proof failed', error));
      return;
    }

    if (event?.id === IMPORT_BOOX_BUTTON_ID) {
      importBooxNativeStroke()
        .then(result => {
          console.log(`InkBridge BOOX import page=${result.page + 1} source=${result.sourceUuid} samples=${result.sampleCount}`);
        })
        .catch(error => console.error('InkBridge BOOX import proof failed', error));
      return;
    }

    if (event?.id === EXPORT_SUPERNOTE_BUTTON_ID) {
      exportCurrentSupernotePage()
        .then(result => {
          console.log(`INKBRIDGE_EXPORT_DONE page=${result.page + 1} strokes=${result.strokeCount} samples=${result.sampleCount} chunks=${result.chunkCount}`);
        })
        .catch(error => console.error('InkBridge page export failed', error));
      return;
    }

    if (event?.id === APPLY_BOOX_RETURN_BUTTON_ID) {
      applyBooxReturnTest()
        .then(result => {
          console.log(
            `INKBRIDGE_RETURN_DONE page=${result.page + 1} modified=${result.modifiedCount} movedReplaced=${result.movedReplacedCount} movedAlreadyCorrect=${result.movedAlreadyCorrectCount} deleted=${result.deletedCount} replaced=${result.replacedCount} inserted=${result.insertedCount} alreadyCorrect=${result.alreadyCorrect}`,
          );
        })
        .catch(error => console.error('INKBRIDGE_RETURN_ERROR', error));
    }
  },
});
