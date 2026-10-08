// Account settings pane. Neuron runs locally as a single-user desktop
// app — there is no cloud account; this pane just surfaces the current
// user + workspace from `me:get`.
import { useMe } from '../../hooks/useMe';

export function AccountPane(): JSX.Element {
  const me = useMe();
  const user = me.data?.user;
  const ws = me.data?.workspace;
  return (
    <>
      <h2 className="text-h2" style={{ marginTop: 0 }}>
        Account
      </h2>
      <p className="text-muted">
        Neuron runs locally as a single-user desktop app — there is no cloud
        account or sign-in.
      </p>
      <div className="set-card">
        <div className="set-row">
          <div>
            <div className="set-row-title">User</div>
            <div className="set-row-sub">
              {me.isLoading
                ? 'Loading…'
                : `${user?.name ?? '—'} · ${user?.initials ?? '··'}`}
            </div>
          </div>
        </div>
        <div className="set-row">
          <div>
            <div className="set-row-title">Workspace</div>
            <div className="set-row-sub">
              {me.isLoading
                ? 'Loading…'
                : `${ws?.name ?? '—'} · ${ws?.count ?? 0} workflows`}
            </div>
          </div>
        </div>
      </div>
    </>
  );
}
