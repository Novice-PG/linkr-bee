import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';

const DOMAIN = 0x4C42;

export default class EntryAbility extends UIAbility {
  onCreate(_want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    hilog.info(DOMAIN, 'LinkrBee', 'EntryAbility created');
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    this.configureImmersiveWindow(windowStage);
    windowStage.loadContent('pages/Index', (error) => {
      if (error.code) {
        hilog.error(
          DOMAIN,
          'LinkrBee',
          'Failed to load Index: %{public}s',
          JSON.stringify(error),
        );
      }
    });
  }

  private async configureImmersiveWindow(
    windowStage: window.WindowStage,
  ): Promise<void> {
    try {
      const mainWindow = windowStage.getMainWindowSync();
      await mainWindow.setWindowLayoutFullScreen(true);
      // Match web/skin-v2.css --bg so the window does not flash white before
      // the WebView paints its first frame.
      await mainWindow.setWindowBackgroundColor('#05080D');
      await mainWindow.setWindowSystemBarProperties({
        statusBarColor: '#00000000',
        navigationBarColor: '#00000000',
        // Transparent bars over a dark page need light glyphs, otherwise the
        // clock and the battery indicator vanish.
        statusBarContentColor: '#E8EEF7',
        navigationBarContentColor: '#E8EEF7',
      });
    } catch (error) {
      hilog.warn(
        DOMAIN,
        'LinkrBee',
        'Failed to configure immersive system bars: %{public}s',
        JSON.stringify(error),
      );
    }
  }
}
