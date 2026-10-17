import { useGlobalSettings, usePluginMessage, useSendToPlugin, useSettings, useStreamDeck } from "@mikanseilaboratory/streamdeck-pi-client";
import { useEffect, useRef, useState } from "react";
import type { AtemConfigBridge, LiveInstance } from "./bridge";
import type {
  ActionSettings,
  AtemStateMessage,
  GlobalSettings,
  InstanceConfig,
  OptionsMessage,
  SelectItem,
  StatusMessage,
  TargetGroup,
  TargetSelector,
} from "./generated/contracts";

const CONFIG_WINDOW = "atem-config";

const EMPTY_SETTINGS: ActionSettings = {
  atem_ip: null,
  target: null,
  me: null,
  video_source: null,
  aux_bus: null,
  key: null,
  on_air: null,
};

const GLOBAL_DEFAULTS: GlobalSettings = {
  instances: [],
  groups: [],
  seeded: false,
  revision: 0,
};

type Settings = ActionSettings & Record<string, unknown>;
type Globals = GlobalSettings & Record<string, unknown>;
type InspectorMessage = AtemStateMessage | OptionsMessage | StatusMessage;
type NumberField = "me" | "video_source" | "aux_bus" | "key";

export function App() {
  const deck = useStreamDeck();
  const action = deck.actionInfo?.action ?? "";
  const { settings, setSettings } = useSettings<Settings>(EMPTY_SETTINGS);
  const global = useGlobalSettings<Globals>(GLOBAL_DEFAULTS);
  const sendToPlugin = useSendToPlugin();
  const [statuses, setStatuses] = useState<LiveInstance[]>([]);
  const [options, setOptions] = useState<Record<string, SelectItem[]>>({});
  const settingsRef = useRef(global.settings);
  const statusRef = useRef(statuses);
  const listenersRef = useRef(new Set<(instances: LiveInstance[]) => void>());
  const configWindowRef = useRef<Window | null>(null);
  const sendRef = useRef(sendToPlugin);
  settingsRef.current = global.settings;
  statusRef.current = statuses;
  sendRef.current = sendToPlugin;

  const instances = global.settings.instances ?? [];
  const groups = global.settings.groups ?? [];
  const target = shownTarget(settings, instances);
  const targetKey = JSON.stringify(target);

  usePluginMessage<InspectorMessage>((message) => {
    if (message.type === "options" && "items" in message) {
      setOptions((previous) => ({ ...previous, [message.kind]: message.items }));
    }
    if (message.type === "status" && "instances" in message) {
      setStatuses(message.instances);
    }
  });

  useEffect(() => {
    const bridge: AtemConfigBridge = {
      getSettings: () => settingsRef.current,
      setSettings: (next) => global.setSettings(next as Globals),
      reconnect: (id) => sendRef.current({ type: "reconnect", id }),
      subscribe: (listener) => {
        listenersRef.current.add(listener);
        listener(statusRef.current);
        return () => listenersRef.current.delete(listener);
      },
    };
    window.atemConfig = bridge;
    return () => {
      if (window.atemConfig === bridge) delete window.atemConfig;
    };
  }, [global.setSettings]);

  useEffect(() => {
    for (const listener of listenersRef.current) listener(statuses);
    const child = configWindowRef.current;
    if (child && !child.closed && child.opener === window) {
      child.refreshAtemConfig?.(settingsRef.current, statuses);
    }
  }, [statuses, global.settings]);

  useEffect(() => {
    sendToPlugin({ type: "ready" });
    const retry = window.setTimeout(() => sendToPlugin({ type: "ready" }), 250);
    return () => window.clearTimeout(retry);
  }, [sendToPlugin]);

  useEffect(() => {
    const selected = JSON.parse(targetKey) as TargetSelector;
    const timer = window.setTimeout(() => {
      for (const kind of kindsFor(action)) {
        sendToPlugin({ type: "requestOptions", kind, atem_ip: settings.atem_ip, target: selected });
      }
    }, 200);
    return () => window.clearTimeout(timer);
  }, [action, sendToPlugin, settings.atem_ip, targetKey]);

  const setNumber = (field: NumberField, raw: string) => {
    setSettings((previous) => ({
      ...previous,
      [field]: raw === "" ? null : Number(raw),
    }));
  };

  const setOnAir = (raw: string) => {
    setSettings((previous) => ({
      ...previous,
      on_air: raw === "" ? null : raw === "true",
    }));
  };

  const fields = fieldsFor(action);

  const openConfig = () => {
    const features = "width=760,height=840";
    const existing = window.open("", CONFIG_WINDOW);
    if (!existing) return;
    let owned = false;
    try {
      owned = existing.opener === window;
    } catch {
      owned = false;
    }
    if (!owned) {
      existing.close();
      configWindowRef.current = window.open("./configuration.html", CONFIG_WINDOW, features);
      return;
    }
    const href = existing.location.href;
    if (href.includes("configuration.html")) {
      existing.focus();
      existing.refreshAtemConfig?.(settingsRef.current, statusRef.current);
      configWindowRef.current = existing;
      return;
    }
    existing.location.href = new URL("./configuration.html", window.location.href).href;
    configWindowRef.current = existing;
  };

  return (
    <div className="sdpi-wrapper">
      <div className="sdpi-heading">Target</div>
      <TargetPicker
        instances={instances}
        groups={groups}
        statuses={statuses}
        value={target}
        onChange={(next) => setSettings((previous) => ({ ...previous, target: next }))}
      />

      <div className="sdpi-heading">ATEM</div>
      {fields.me && (
        <SelectField
          label="ME (Mix Effect) index"
          items={withCurrent(options.me, settings.me)}
          value={asText(settings.me)}
          onChange={(value) => setNumber("me", value)}
        />
      )}
      {fields.aux && (
        <SelectField
          label="AUX bus"
          items={withCurrent(options.aux, settings.aux_bus)}
          value={asText(settings.aux_bus)}
          onChange={(value) => setNumber("aux_bus", value)}
        />
      )}
      {fields.dsk && (
        <SelectField
          label="DSK key"
          items={withCurrent(options.dsk, settings.key)}
          value={asText(settings.key)}
          onChange={(value) => setNumber("key", value)}
        />
      )}
      {fields.video && (
        <SelectField
          label={fields.aux ? "Video source" : "Video source (for program/preview)"}
          items={withCurrent(fields.aux ? options.auxVideo : options.video, settings.video_source)}
          value={asText(settings.video_source)}
          onChange={(value) => setNumber("video_source", value)}
        />
      )}
      {fields.onAir && (
        <div className="sdpi-item">
          <div className="sdpi-item-label">On key press</div>
          <select className="sdpi-item-value select" value={onAirValue(settings.on_air)} onChange={(event) => setOnAir(event.target.value)}>
            <option value="">Toggle (current state)</option>
            <option value="true">Set On Air</option>
            <option value="false">Set Off Air</option>
          </select>
        </div>
      )}

      <div className="sdpi-heading">Connections</div>
      <div className="sdpi-item">
        <div className="sdpi-item-label">ATEM</div>
        <button className="sdpi-item-value" type="button" onClick={openConfig}>
          Manage instances
        </button>
      </div>
    </div>
  );
}

function TargetPicker({
  instances,
  groups,
  statuses,
  value,
  onChange,
}: {
  instances: InstanceConfig[];
  groups: TargetGroup[];
  statuses: LiveInstance[];
  value: TargetSelector;
  onChange: (value: TargetSelector) => void;
}) {
  const [preferMultiple, setPreferMultiple] = useState(value.kind === "instances" && value.ids.length !== 1);
  useEffect(() => {
    if (value.kind !== "instances") setPreferMultiple(false);
  }, [value.kind]);
  const mode = preferMultiple ? "multiple" : targetMode(value);
  const selected = value.kind === "instances" ? value.ids : [];
  const missingGroup = value.kind === "group" && !groups.some((group) => group.id === value.id);
  const missingInstance =
    value.kind === "instances" && value.ids.length === 1 && !instances.some((instance) => instance.id === value.ids[0]);
  return (
    <>
      <div className="sdpi-item">
        <div className="sdpi-item-label">Send to</div>
        <select
          className="sdpi-item-value select"
          value={mode}
          onChange={(event) => {
            const next = event.target.value;
            setPreferMultiple(next === "multiple");
            onChange(selectorFromMode(next, selected));
          }}
        >
          <option value="all">All enabled</option>
          {groups.map((group) => (
            <option key={group.id} value={`group:${group.id}`}>
              Group: {group.name || "Untitled"}
            </option>
          ))}
          {missingGroup && value.kind === "group" && <option value={`group:${value.id}`}>Missing group</option>}
          {instances.map((instance) => (
            <option key={instance.id} value={`instance:${instance.id}`}>
              {instance.name || instance.host}
              {instance.enabled ? "" : " (disabled)"}
            </option>
          ))}
          {missingInstance && value.kind === "instances" && (
            <option value={`instance:${value.ids[0]}`}>Missing switcher</option>
          )}
          <option value="multiple">Multiple switchers</option>
        </select>
      </div>
      {mode === "multiple" && (
        <div className="sdpi-item targets" {...{ type: "checkbox" }}>
          <div className="sdpi-item-label">Switchers</div>
          <div className="sdpi-item-value">
            {instances.length === 0 && <span>No switchers yet</span>}
            {instances.map((instance) => {
              const id = `target-${instance.id}`;
              const on = selected.includes(instance.id);
              const status = statuses.find((item) => item.id === instance.id)?.status?.kind;
              return (
                <div className="sdpi-item-child" key={instance.id}>
                  <input
                    id={id}
                    type="checkbox"
                    checked={on}
                    onChange={() => {
                      const ids = on ? selected.filter((item) => item !== instance.id) : [...selected, instance.id];
                      onChange({ kind: "instances", ids });
                    }}
                  />
                  <label htmlFor={id}>
                    <span></span>
                    <i className="swatch" style={{ background: statusColor(status, instance.color) }} />
                    {instance.name || instance.host}
                  </label>
                </div>
              );
            })}
          </div>
        </div>
      )}
    </>
  );
}

function targetMode(value: TargetSelector): string {
  if (value.kind === "all") return "all";
  if (value.kind === "group") return `group:${value.id}`;
  if (value.ids.length === 1) return `instance:${value.ids[0]}`;
  return "multiple";
}

function selectorFromMode(mode: string, selected: string[]): TargetSelector {
  if (mode === "all") return { kind: "all" };
  if (mode === "multiple") return { kind: "instances", ids: selected };
  if (mode.startsWith("group:")) return { kind: "group", id: mode.slice("group:".length) };
  if (mode.startsWith("instance:")) return { kind: "instances", ids: [mode.slice("instance:".length)] };
  return { kind: "all" };
}

function statusColor(kind: string | undefined, fallback: string): string {
  switch (kind) {
    case "connected":
      return "#3cba7a";
    case "connecting":
      return "#e2b15a";
    case "disconnected":
      return "#8a4a3a";
    case "disabled":
      return "#6d7890";
    default:
      return fallback;
  }
}

function shownTarget(settings: ActionSettings, instances: InstanceConfig[]): TargetSelector {
  if (settings.target) return settings.target;
  const host = (settings.atem_ip ?? "").trim();
  if (!host) return { kind: "all" };
  const ids = instances.filter((item) => item.host.trim() === host).map((item) => item.id);
  if (ids.length > 0) return { kind: "instances", ids };
  return { kind: "all" };
}

function SelectField({
  label,
  items,
  value,
  onChange,
}: {
  label: string;
  items: SelectItem[] | undefined;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <div className="sdpi-item">
      <div className="sdpi-item-label">{label}</div>
      <select className="sdpi-item-value select" value={value} onChange={(event) => onChange(event.target.value)}>
        <option value="">Select…</option>
        {(items ?? []).map((item) => (
          <option key={item.value} value={item.value}>
            {item.label}
          </option>
        ))}
      </select>
    </div>
  );
}

function fieldsFor(action: string) {
  return {
    me: action.endsWith(".cut") || action.endsWith(".auto") || action.endsWith(".programinput") || action.endsWith(".previewinput") || action.endsWith(".cuttoblack") || action.endsWith(".fadetoblack"),
    video: action.endsWith(".programinput") || action.endsWith(".previewinput") || action.endsWith(".dskcut") || action.endsWith(".dskfill"),
    aux: action.endsWith(".auxoutput"),
    dsk: action.includes(".dsk"),
    onAir: action.endsWith(".dskonair"),
  };
}

function kindsFor(action: string): string[] {
  const fields = fieldsFor(action);
  const kinds: string[] = [];
  if (fields.me) kinds.push("me");
  if (fields.aux) kinds.push("aux", "auxVideo");
  if (fields.dsk) kinds.push("dsk");
  if (fields.video && !fields.aux) kinds.push("video");
  return kinds;
}

function asText(value: number | null): string {
  return value == null ? "" : String(value);
}

function onAirValue(value: boolean | null): string {
  if (value === true) return "true";
  if (value === false) return "false";
  return "";
}

function withCurrent(items: SelectItem[] | undefined, current: number | null): SelectItem[] {
  const list = items ?? [];
  if (current == null) return list;
  const value = String(current);
  if (list.some((item) => item.value === value)) return list;
  return [{ label: value, value }, ...list];
}
