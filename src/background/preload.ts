import { BackgroundApi, UserSettings } from '../shared.interfaces';
import { Channel, REPLY_BY_CHANNEL } from './constants';

const { contextBridge, ipcRenderer } = require('electron');

const api: BackgroundApi = {
  enableAutostart: () => {
    ipcRenderer.send(Channel.ENABLE_AUTOSTART);
  },
  disableAutostart: () => {
    ipcRenderer.send(Channel.DISABLE_AUTOSTART);
  },
  autostartTaskExist: () => {
    return new Promise((resolve, reject) => {
      ipcRenderer.send(Channel.GET_AUTOSTART_STATUS);
      ipcRenderer.on(REPLY_BY_CHANNEL[Channel.GET_AUTOSTART_STATUS], (e, result, error) => {
        if (error) {
          return reject(error);
        }
        resolve(result != null);
      });
    });
  },
  getUserSettings: async (route?: string) => {
    return new Promise<UserSettings>((resolve, reject) => {
      ipcRenderer.send(Channel.GET_USER_SETTINGS, route);
      ipcRenderer.on(REPLY_BY_CHANNEL[Channel.GET_USER_SETTINGS], (e, result: UserSettings, error) => {
        if (error) {
          return reject(error);
        }
        resolve(result);
      });
    });
  },
  saveUserSettings(settings) {
    return new Promise<void>((resolve, reject) => {
      ipcRenderer.send(Channel.SAVE_USER_SETTINGS, settings);
      ipcRenderer.on(REPLY_BY_CHANNEL[Channel.SAVE_USER_SETTINGS], (e, result, error) => {
        if (error) {
          return reject(error);
        }
        resolve();
      });
    });
  },
};

contextBridge.exposeInMainWorld('backgroundApi', api);