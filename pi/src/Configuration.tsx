import { useEffect, useRef, useState } from "react";
import type { LiveInstance } from "./bridge";
import type { GlobalSettings, InstanceConfig, TargetGroup } from "./generated/contracts";

const PALETTE = ["#4c8dff", "#ef5b5b", "#3cba7a", "#e2b15a", "#b07cff", "#4ec8d4", "#f08bbd", "#9aa4b5"];

const emptySettings = (): GlobalSettings => ({
  instances: [],
  groups: [],
  seeded: true,
  revision: 0,
});

export function Configuration() {
  const [settings, setSettings] = useState<GlobalSettings>(emptySettings);
  const [statuses, setStatuses] = useState<LiveInstance[]>([]);
  const [ready, setReady] = useState(false);
  const [missing, setMissing] = useState(false);
  const settingsRef = useRef(settings);

  const commit = (recipe: (current: GlobalSettings) => GlobalSettings) => {
    const next = recipe(settingsRef.current);
    settingsRef.current = next;
    setSettings(next);
    window.opener?.atemConfig?.setSettings(next);
  };

  useEffect(() => {
    window.refreshAtemConfig = (next, instances) => {
      settingsRef.current = next;
      setSettings(next);
      setStatuses(instances);
      setReady(true);
    };
    if (!window.opener) {
      setMissing(true);
      return () => {
        delete window.refreshAtemConfig;
      };
    }
    let unsubscribe = () => {};
    const attach = () => {
      const bridge = window.opener?.atemConfig;
      if (!bridge) return false;
      const loaded = bridge.getSettings();
      settingsRef.current = loaded;
      setSettings(loaded);
      unsubscribe = bridge.subscribe(setStatuses);
      setReady(true);
      return true;
    };
    if (attach()) {
      return () => {
        unsubscribe();
        delete window.refreshAtemConfig;
      };
    }
    const timer = window.setInterval(() => {
      if (attach()) window.clearInterval(timer);
    }, 200);
    return () => {
      window.clearInterval(timer);
      unsubscribe();
      delete window.refreshAtemConfig;
    };
  }, []);

  const instances = settings.instances ?? [];

  if (missing) {
    return (
      <main>
        <header className="config-header">
          <h1>ATEM [MikanseiLaboratory]</h1>
        </header>
        <p className="empty-note">Open this window from a key's property inspector.</p>
      </main>
    );
  }

  return (
    <main>
      <header className="config-header">
        <h1>ATEM [MikanseiLaboratory]</h1>
      </header>
      <div className="config-body">
        <div className="sdpi-heading">ATEM instances</div>
        {!ready && <p className="empty-note">Waiting for the property inspector…</p>}
        {ready && instances.length === 0 && (
          <p className="empty-note">No ATEM instances yet. Add one to start controlling it from a key.</p>
        )}
        <div className="obs-grid">
          {instances.map((instance, index) => (
            <InstanceCard
              key={instance.id}
              instance={instance}
              index={index}
              status={statuses.find((item) => item.id === instance.id)}
              onChange={(next) =>
                commit((current) => ({
                  ...current,
                  instances: current.instances.map((item) => (item.id === instance.id ? next : item)),
                }))
              }
              onRemove={() =>
                commit((current) => ({
                  ...current,
                  instances: current.instances.filter((item) => item.id !== instance.id),
                  groups: (current.groups ?? []).map((group) => ({
                    ...group,
                    members: (group.members ?? []).filter((id) => id !== instance.id),
                  })),
                }))
              }
              onReconnect={() => window.opener?.atemConfig?.reconnect(instance.id)}
              onDrop={(from) => {
                if (from === index) return;
                commit((current) => {
                  const copy = current.instances.slice();
                  const [moved] = copy.splice(from, 1);
                  if (!moved) return current;
                  copy.splice(index, 0, moved);
                  return { ...current, instances: copy };
                });
              }}
            />
          ))}
        </div>
        <div className="card-actions">
          <button
            type="button"
            onClick={() =>
              commit((current) => ({
                ...current,
                instances: [
                  ...current.instances,
                  {
                    id: crypto.randomUUID(),
                    name: `ATEM ${current.instances.length + 1}`,
                    host: "",
                    port: 9910,
                    color: PALETTE[current.instances.length % PALETTE.length],
                    enabled: true,
                  },
                ],
              }))
            }
          >
            Add ATEM
          </button>
          <button type="button" onClick={() => window.opener?.atemConfig?.reconnect()}>
            Reconnect all
          </button>
        </div>
        <Groups
          instances={instances}
          groups={settings.groups ?? []}
          setGroups={(recipe) => commit((current) => ({ ...current, groups: recipe(current.groups ?? []) }))}
        />
      </div>
    </main>
  );
}

function InstanceCard({
  instance,
  index,
  status,
  onChange,
  onRemove,
  onReconnect,
  onDrop,
}: {
  instance: InstanceConfig;
  index: number;
  status?: LiveInstance;
  onChange: (instance: InstanceConfig) => void;
  onRemove: () => void;
  onReconnect: () => void;
  onDrop: (from: number) => void;
}) {
  const kind = instance.enabled ? status?.status?.kind : "disabled";
  return (
    <section
      className="obs-card"
      onDragOver={(event) => event.preventDefault()}
      onDrop={(event) => {
        const from = Number(event.dataTransfer.getData("text/plain"));
        if (!Number.isNaN(from)) onDrop(from);
      }}
    >
      <div className="obs-card-head">
        <span
          className="drag-handle"
          draggable
          title="Drag to reorder"
          onDragStart={(event) => event.dataTransfer.setData("text/plain", String(index))}
        >
          :::
        </span>
        <span className={`status-dot ${kind ?? ""}`} title={kind ?? "unknown"} />
        <strong>{instance.name || "ATEM"}</strong>
        <span className="status-text">{statusLabel(kind, status?.status)}</span>
        <input type="color" value={instance.color} aria-label="Color" onChange={(event) => onChange({ ...instance, color: event.target.value })} />
      </div>
      <label className="field">
        Name
        <input value={instance.name} onChange={(event) => onChange({ ...instance, name: event.target.value })} />
      </label>
      <label className="field">
        Host
        <input value={instance.host} placeholder="192.168.1.100" onChange={(event) => onChange({ ...instance, host: event.target.value })} />
      </label>
      <label className="field">
        Port
        <input type="number" value={instance.port} onChange={(event) => onChange({ ...instance, port: Number(event.target.value) })} />
      </label>
      <div className="sdpi-item" {...{ type: "checkbox" }}>
        <div className="sdpi-item-label">Enabled</div>
        <input
          id={`enabled-${instance.id}`}
          className="sdpi-item-value"
          type="checkbox"
          checked={instance.enabled}
          onChange={(event) => onChange({ ...instance, enabled: event.target.checked })}
        />
        <label htmlFor={`enabled-${instance.id}`}>
          <span></span>
        </label>
      </div>
      <div className="card-actions">
        <button type="button" onClick={onReconnect}>
          Reconnect
        </button>
        <button type="button" onClick={onRemove}>
          Remove {index + 1}
        </button>
      </div>
    </section>
  );
}

function Groups({
  instances,
  groups,
  setGroups,
}: {
  instances: InstanceConfig[];
  groups: TargetGroup[];
  setGroups: (recipe: (groups: TargetGroup[]) => TargetGroup[]) => void;
}) {
  return (
    <>
      <div className="sdpi-heading">Groups</div>
      {groups.map((group) => (
        <section className="obs-card" key={group.id}>
          <label className="field">
            Name
            <input
              value={group.name}
              onChange={(event) => {
                const name = event.target.value;
                setGroups((current) => current.map((item) => (item.id === group.id ? { ...item, name } : item)));
              }}
            />
          </label>
          <div className="sdpi-item targets" {...{ type: "checkbox" }}>
            <div className="sdpi-item-label">Members</div>
            <div className="sdpi-item-value">
              {instances.length === 0 && <span>No switchers yet</span>}
              {instances.map((instance) => {
                const id = `group-${group.id}-${instance.id}`;
                const members = group.members ?? [];
                const on = members.includes(instance.id);
                return (
                  <div className="sdpi-item-child" key={instance.id}>
                    <input
                      id={id}
                      type="checkbox"
                      checked={on}
                      onChange={(event) => {
                        const include = event.target.checked;
                        setGroups((current) =>
                          current.map((item) => {
                            if (item.id !== group.id) return item;
                            const previous = item.members ?? [];
                            const next = include
                              ? previous.includes(instance.id)
                                ? previous
                                : [...previous, instance.id]
                              : previous.filter((member) => member !== instance.id);
                            return { ...item, members: next };
                          }),
                        );
                      }}
                    />
                    <label htmlFor={id}>
                      <span></span>
                      <i className="swatch" style={{ background: instance.color }} />
                      {instance.name || instance.host}
                    </label>
                  </div>
                );
              })}
            </div>
          </div>
          <div className="card-actions">
            <button type="button" onClick={() => setGroups((current) => current.filter((item) => item.id !== group.id))}>
              Remove group
            </button>
          </div>
        </section>
      ))}
      <div className="card-actions">
        <button
          type="button"
          onClick={() =>
            setGroups((current) => [
              ...current,
              { id: crypto.randomUUID(), name: `Group ${current.length + 1}`, members: [] },
            ])
          }
        >
          Add group
        </button>
      </div>
    </>
  );
}

function statusLabel(kind: string | undefined, status: LiveInstance["status"]) {
  if (kind === "disconnected" && status && "message" in status && status.message) return status.message;
  if (kind === "connected") return "Connected";
  if (kind === "connecting") return "Connecting";
  if (kind === "disabled") return "Disabled";
  return kind ?? "Unknown";
}
