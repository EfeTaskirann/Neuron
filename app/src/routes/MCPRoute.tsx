// Ports `Neuron Design/app/routes.jsx::MCPRoute`. Server catalog
// from `data.servers` → `useServers()`. ServerCard / ServerRow are
// internal helpers kept in this file; promote when a second
// consumer needs them (none planned for Week 2).
import { useState } from 'react';
import { NIcon, NodeGlyph } from '../components/icons';
import { useServers } from '../hooks/useServers';
import { useMcpInstall, useMcpUninstall } from '../hooks/mutations';
import type { Server } from '../lib/bindings';

type McpFilter = 'all' | 'official' | 'community' | 'installed';
const FILTERS: McpFilter[] = ['all', 'official', 'community', 'installed'];

export function MCPRoute(): JSX.Element {
  const { data: servers = [], isLoading, isError, error } = useServers();
  const [search, setSearch] = useState('');
  const [filter, setFilter] = useState<McpFilter>('all');
  if (isLoading) {
    return <div className="route route-mcp route-loading">Loading servers…</div>;
  }
  if (isError) {
    throw error instanceof Error ? error : new Error(String(error));
  }
  const q = search.trim().toLowerCase();
  // "official" maps to the curated/featured entries and "community" to
  // the rest; "installed" is the explicit per-server flag. Search
  // matches name / description / author substring.
  const visible = servers.filter((s) => {
    if (filter === 'installed' && !s.installed) return false;
    if (filter === 'official' && !s.featured) return false;
    if (filter === 'community' && s.featured) return false;
    if (q && !`${s.name} ${s.desc} ${s.by}`.toLowerCase().includes(q)) {
      return false;
    }
    return true;
  });
  const featured = visible.filter((s) => s.featured);
  return (
    <div className="route route-mcp">
      <div className="mcp-search">
        <NIcon name="search" size={16} />
        <input
          placeholder={`Search ${servers.length} servers…`}
          value={search}
          onChange={(e) => setSearch(e.target.value)}
        />
        <div className="chip-row mcp-chips">
          {FILTERS.map((c) => (
            <button
              key={c}
              type="button"
              className={`chip${filter === c ? ' active' : ''}`}
              onClick={() => setFilter(c)}
            >
              {c}
            </button>
          ))}
        </div>
      </div>

      {servers.length === 0 ? (
        <div className="mcp-empty">
          <h3 className="route-section-title">No servers in the catalog</h3>
          <p className="text-muted">
            The MCP marketplace is empty right now. Check back once servers
            are published.
          </p>
        </div>
      ) : (
        <>
          {featured.length > 0 && (
            <>
              <h3 className="route-section-title">Featured</h3>
              <div className="mcp-featured">
                {featured.map((s) => (
                  <ServerCard key={s.id} s={s} featured />
                ))}
              </div>
            </>
          )}

          <h3 className="route-section-title">All servers</h3>
          {visible.length > 0 ? (
            <div className="mcp-list">
              {visible.map((s) => (
                <ServerRow key={s.id} s={s} />
              ))}
            </div>
          ) : (
            <p className="text-muted mcp-empty">
              No servers match your search{filter !== 'all' ? ` in “${filter}”` : ''}.
            </p>
          )}
        </>
      )}
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
