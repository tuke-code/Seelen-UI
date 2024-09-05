import { getRootContainer } from '../shared';
import { wrapConsole } from '../shared/ConsoleWrapper';
import { App } from './App';
import { registerDocumentEvents } from './events';
import { initStore, store } from './store';
import { createRoot } from 'react-dom/client';
import { Provider } from 'react-redux';
import { declareDocumentAsLayeredHitbox } from 'seelen-core';

import './styles/reset.css';
import './styles/colors.css';

async function Main() {
  wrapConsole();
  await declareDocumentAsLayeredHitbox();
  await initStore();
  registerDocumentEvents();

  createRoot(getRootContainer()).render(
    <Provider store={store}>
      <App />
    </Provider>,
  );
}

Main();
