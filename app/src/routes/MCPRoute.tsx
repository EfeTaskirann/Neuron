// MCP servers Neuron can install and launch: the bundled catalog
// (`src-tauri/src/mcp/manifests`), which lists only servers with a
// launch recipe — no invented install counts, ratings or "featured"
// curation. ServerRow is an internal helper kept in this file.
import { useState } from 'react';
import { confirmWarning } from '../lib/confirm';
import { NIcon, NodeGlyph } from '../components/icons';
import { useServers } from '../hooks/useServers';
import { useMcpInstall, useMcpUninstall } from '../hooks/mutations';
import type { Server } from '../lib/bindings';

type McpFilter = 'all' | 'installed';
const FILTERS: McpFilter[] = ['all', 'installed'];

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
  // Search matches name / description / author substring.
  const visible = servers.filter((s) => {
    if (filter === 'installed' && !s.installed) return false;
    if (q && !`${s.name} ${s.desc} ${s.by}`.toLowerCase().includes(q)) {
      return false;
    }
    return true;
  });
  return (
    <div className="route route-mcp">
      <p className="text-muted mcp-intro">
        MCP servers Neuron can install and launch. Installing one starts it once to
        check that it works and records its tools. Swarm agents don't use these
        servers yet.
      </p>
      <div className="mcp-search">
        <NIcon name="search" size={16} />
        <input
          aria-label="Search MCP servers"
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
          <p className="text-muted">This build ships no MCP servers.</p>
        </div>
      ) : visible.length > 0 ? (
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
      {s.installed ? (
        <button
          className="btn ghost sm"
          disabled={busy}
          onClick={async () => {
            const remove = await confirmWarning({
              title: 'Sunucuyu kaldır',
              okLabel: 'Kaldır',
              message: `"${s.name}" kaldırılsın mı? Araçları ajanlara artık sunulmaz.`,
            });
            if (remove) uninstall.mutate(s.id);
          }}
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
