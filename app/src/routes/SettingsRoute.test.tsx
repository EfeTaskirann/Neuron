import { describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';

// T1-01 render-smoke for the `settings` tab, which had none. The route
// has no query deps; its only stateful child is the Appearance pane via
// `useAppearance`. Mock the hook to a fixed state (keep ACCENT_SWATCHES
// real) so the test asserts structure without touching localStorage or
// mutating the <html> element.
vi.mock('../hooks/useAppearance', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../hooks/useAppearance')>();
  return {
    ...actual,
    useAppearance: vi.fn(() => ({
      theme: 'dark' as const,
      accent: '#a874d6',
      density: 'comfortable' as const,
      motion: 'full' as const,
      setTheme: vi.fn(),
      setAccent: vi.fn(),
      setDensity: vi.fn(),
      setMotion: vi.fn(),
    })),
  };
});

// The Keys pane drives the secrets:* keychain commands.
vi.mock('../lib/bindings', () => ({
  commands: {
    secretsHas: vi.fn(),
    secretsSet: vi.fn(),
    secretsDelete: vi.fn(),
  },
}));

import { SettingsRoute } from './SettingsRoute';

describe('SettingsRoute', () => {
  it('renders the section nav and defaults to the Appearance pane', () => {
    render(<SettingsRoute />);
    // Section nav buttons.
    expect(screen.getByRole('button', { name: /account/i })).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: /appearance/i }),
    ).toBeInTheDocument();
    // Appearance pane is active by default — its labelled radiogroups render.
    expect(screen.getByRole('radiogroup', { name: /theme/i })).toBeInTheDocument();
    expect(
      screen.getByRole('radiogroup', { name: /accent color/i }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole('radiogroup', { name: /density/i }),
    ).toBeInTheDocument();
    expect(screen.getByRole('radiogroup', { name: /motion/i })).toBeInTheDocument();
  });

  it('switches to a placeholder pane when a non-appearance section is picked', () => {
    render(<SettingsRoute />);
    fireEvent.click(screen.getByRole('button', { name: /workflows/i }));
    expect(screen.getByText(/settings for this section/i)).toBeInTheDocument();
    // The appearance radiogroups are gone once another section is active.
    expect(
      screen.queryByRole('radiogroup', { name: /theme/i }),
    ).not.toBeInTheDocument();
  });

  it('renders the Keys pane and saves an API key via the keychain', async () => {
    const { commands } = await import('../lib/bindings');
    vi.mocked(commands.secretsHas).mockResolvedValue({ status: 'ok', data: false });
    vi.mocked(commands.secretsSet).mockResolvedValue({ status: 'ok', data: null });
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    render(
      <QueryClientProvider client={qc}>
        <SettingsRoute />
      </QueryClientProvider>,
    );
    fireEvent.click(screen.getByRole('button', { name: /keys/i }));
    await waitFor(() =>
      expect(screen.getByText('Anthropic (Claude)')).toBeInTheDocument(),
    );
    fireEvent.change(screen.getByLabelText(/anthropic \(claude\) api key/i), {
      target: { value: 'sk-ant-test' },
    });
    fireEvent.click(screen.getAllByRole('button', { name: /^save$/i })[0]!);
    await waitFor(() =>
      expect(commands.secretsSet).toHaveBeenCalledWith('anthropic', 'sk-ant-test'),
    );
  });
});
