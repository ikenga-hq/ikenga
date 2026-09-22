// {{name}} — entry module. The index.html bootstrap loads this file
// dynamically (see comment there for why). Once we're here, we have a
// real URL and relative imports work normally.

import { connectBridge, hostNavigate, isStandalone } from './lib/bridge.js';

const root = document.getElementById('root');

function setStatus(text, cls = 'status') {
  root.innerHTML = `<h1>{{name}}</h1><p class="${cls}">${text}</p>`;
}

setStatus('Connecting…', 'boot');

if (isStandalone()) {
  setStatus('Standalone (no shell connected)');
} else {
  try {
    const ctx = await connectBridge({ name: '{{name}}', version: '{{version}}' });
    setStatus('Connected. Theme=' + (ctx?.theme ?? '?'));
  } catch (e) {
    setStatus('Bridge connect failed: ' + (e?.message ?? e), 'err');
  }
}

// Example cross-pkg link — wire up your own UI here.
// document.getElementById('go-suite').addEventListener('click', () => {
//   hostNavigate('/pkg/com.ikenga.suite/');
// });
