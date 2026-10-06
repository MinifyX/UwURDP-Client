import React from 'react';
import ReactDOM from 'react-dom/client';
import { App } from './App';
import { prepareDocument } from './lib/appearance';
import { loadBuildInfo } from './lib/build';
import './styles/index.css';

prepareDocument();

const root = document.getElementById('root');
if (!root) throw new Error('#root missing from index.html');

// What this build can do (GitHub or Mac App Store) decides what the page shows; asking
// takes a moment and never fails, so the first render waits for it.
void loadBuildInfo().then(() =>
  ReactDOM.createRoot(root).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  ),
);
