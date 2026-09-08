import { invoke, SeelenCommand, SeelenEvent, Settings, subscribe } from "@seelen-ui/lib";
import type { MediaDevice, PhysicalMonitor, RadioDevice } from "@seelen-ui/lib/types";
import { lazyRune } from "libs/ui/svelte/utils";
import { locale } from "./i18n/index.ts";

const settings = lazyRune(() => Settings.getAsync());
Settings.onChange((s) => (settings.value = s));
await settings.init();

$effect.root(() => {
  $effect(() => {
    locale.set(settings.value.language);
  });
});

// Initialize lazy signals
const brightness = lazyRune(() => invoke(SeelenCommand.GetAllMonitorsBrightness));
subscribe(SeelenEvent.SystemMonitorsBrightnessChanged, brightness.setByPayload);

const mediaDevices = lazyRune(async () => {
  const [inputs, outputs] = await invoke(SeelenCommand.GetMediaDevices);
  return { inputs, outputs };
});
subscribe(SeelenEvent.MediaDevices, ({ payload: [inputs, outputs] }) => {
  mediaDevices.value = { inputs, outputs };
});

const radios = lazyRune(() => invoke(SeelenCommand.GetRadios));
subscribe(SeelenEvent.RadiosChanged, radios.setByPayload);

const monitors = lazyRune(() => invoke(SeelenCommand.SystemGetMonitors));
subscribe(SeelenEvent.SystemMonitorsChanged, monitors.setByPayload);

await Promise.all([brightness.init(), mediaDevices.init(), radios.init(), monitors.init()]);

class State {
  get brightness() {
    return brightness.value;
  }

  get mediaInputs(): MediaDevice[] {
    return mediaDevices.value.inputs;
  }
  get mediaOutputs(): MediaDevice[] {
    return mediaDevices.value.outputs;
  }
  get radios(): RadioDevice[] {
    return radios.value;
  }
  get monitors(): PhysicalMonitor[] {
    return monitors.value;
  }
}
export const state = new State();
