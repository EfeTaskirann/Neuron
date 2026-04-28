// WP-W2-01 placeholder. Single-page Hello surface that proves the
// Tauri 2 + Vite + React harness is wired and the dark-mode tokens
// from `colors_and_type.css` resolve. Real routes land in WP-W2-08.
export function App(): JSX.Element {
  return (
    <main className="app-hello-surface">
      <h1 className="app-hello">Hello Neuron</h1>
    </main>
  );
}
