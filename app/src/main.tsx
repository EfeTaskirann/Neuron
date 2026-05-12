import React from 'react';
import ReactDOM from 'react-dom/client';
import { QueryClientProvider } from '@tanstack/react-query';
import { App } from './App';
import { queryClient } from './lib/queryClient';
import './styles/colors_and_type.css';
import './styles/app.css';
import './styles/canvas.css';
import './styles/terminal.css';
import './styles/swarm.css';
import './styles/swarm-term.css';

const rootEl = document.getElementById('root');
if (!rootEl) {
  throw new Error('Neuron mount point #root not found in index.html');
}

ReactDOM.createRoot(rootEl).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>
  </React.StrictMode>,
);
