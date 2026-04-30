import { useQuery } from '@tanstack/react-query';
import { commands, type Server } from '../lib/bindings';
import { unwrap } from '../lib/unwrap';

// `mcp:list` — MCP server catalog. The mock shape is `data.servers`,
// kept verbatim per ADR-0005.
export function useServers() {
  return useQuery<Server[]>({
    queryKey: ['servers'],
    queryFn: () => unwrap(commands.mcpList()),
  });
}
