import '@fontsource-variable/manrope';
import React from 'react';
import ReactDOM from 'react-dom/client';
import { App } from './App';
import './components/nyu/nyu.css';
import { applyAppearance } from './lib/settings';
import './styles/app.css';
import './styles/features.css';
import './styles/rdp.css';
import './styles/tokens.css';

// Dark by default, as the concept says; Settings → Appearance switches to light
// or follows the system, and decides about animations.
applyAppearance();

const root = document.getElementById('root');
if (!root) throw new Error('#root missing from index.html');

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
