// Ports `Neuron Design/app/routes.jsx::MCPRoute`. Server catalog
// from `data.servers` → `useServers()`. ServerCard / ServerRow are
// internal helpers kept in this file; promote when a second
// consumer needs them (none planned for Week 2).
import { NIcon, NodeGlyph } from '../components/icons';
import { useServers } from '../hooks/useServers';
import { useMcpInstall, useMcpUninstall } from '../hooks/mutations';
import type { Server } from '../lib/bindings';

export function MCPRoute(): JSX.Element {
  const { data: servers = [], isLoading, isError, error } = useServers();
  if (isLoading) {
    return <div className="route route-mcp route-loading">Loading servers…</div>;
  }
  if (isError) {
    throw error instanceof Error ? error : new Error(String(error));
  }
  const featured = servers.filter((s) => s.featured);
  return (
    <div className="route route-mcp">
      <div className="mcp-search">
        <NIcon name="search" size={16} />
        <input placeholder={`Search ${servers.length} servers…`} />
        <div className="chip-row mcp-chips">
          {(['all', 'official', 'community', 'installed'] as const).map((c, i) => (
            <button key={c} className={`chip${i === 0 ? ' active' : ''}`}>
              {c}
            </button>
          ))}
        </div>
      </div>

      <h3 className="route-section-title">Featured</h3>
      <div className="mcp-featured">
        {featured.map((s) => (
          <ServerCard key={s.id} s={s} featured />
        ))}
      </div>

      <h3 className="route-section-title">All servers</h3>
      <div className="mcp-list">
        {servers.map((s) => (
          <ServerRow key={s.id} s={s} />
        ))}
      </div>
    </div>
  );
}

function ServerCard({ s, featured }: { s: Server; featured?: boolean }): JSX.Element {
  const install = useMcpInstall();
  const uninstall = useMcpUninstall();
  const busy = install.isPending || uninstall.isPending;
  return (
    <div className={`server-card${featured ? ' featured' : ''}`}>
      <div className="server-card-head">
        <div className="server-icon">
          <NodeGlyph kind="mcp" size={22} />
        </div>
        <div>
          <div className="server-name">{s.name}</div>
          <div className="server-by">{s.by}</div>
        </div>
        <div className="server-spacer" />
        {s.installed ? (
          <button
            className="btn ghost sm"
            disabled={busy}
            onClick={() => uninstall.mutate(s.id)}
            title="Uninstall server"
          >
            <span className="pill st-installed">installed</span>
          </button>
        ) : (
          <button
            className="btn primary sm"
            disabled={busy}
            onClick={() => install.mutate(s.id)}
          >
            <NIcon name="download" size={11} />
            <span>{install.isPending ? 'Installing…' : 'Install'}</span>
          </button>
        )}
      </div>
      <div className="server-desc">{s.desc}</div>
      <div className="server-foot">
        <span>
          <NIcon name="download" size={11} /> {(s.installs / 1000).toFixed(1)}k
        </span>
        <span>
          <NIcon name="star" size={11} /> {s.rating}
        </span>
      </div>
    </div>
  );
}

function ServerRow({ s }: { s: Server }): JSX.Element {
  const install = useMcpInstall();
  const uninstall = useMcpUninstall();
  const busy = install.isPending || uninstall.isPending;
  return (
    <div className="server-row">
      <div className="server-icon sm">
        <NodeGlyph kind="mcp" size={16} />
      </div>
      <div className="sr-meta">
        <div className="sr-line">
          <span className="sr-name">{s.name}</span>
          <span className="sr-by">{s.by}</span>
        </div>
        <div className="sr-desc">{s.desc}</div>
      </div>
      <div className="sr-stats">
        <span>{(s.installs / 1000).toFixed(1)}k</span>
        <span>★ {s.rating}</span>
      </div>
      {s.installed ? (
        <button
          className="btn ghost sm"
          disabled={busy}
          onClick={() => uninstall.mutate(s.id)}
          title="Uninstall server"
        >
          <span className="pill st-installed">installed</span>
        </button>
      ) : (
        <button
          className="btn ghost sm"
          disabled={busy}
          onClick={() => install.mutate(s.id)}
        >
          {install.isPending ? 'Installing…' : 'Install'}
        </button>
      )}
    </div>
  );
}
