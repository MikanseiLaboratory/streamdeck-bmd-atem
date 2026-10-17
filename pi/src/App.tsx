import { usePluginMessage, useSendToPlugin, useSettings, useStreamDeck } from "@mikanseilaboratory/streamdeck-pi-client";
import { useEffect, useState } from "react";
import type { ActionSettings, AtemStateMessage, OptionsMessage, SelectItem } from "./generated/contracts";

const EMPTY_SETTINGS: ActionSettings = {
  atem_ip: null,
  me: null,
  video_source: null,
  aux_bus: null,
  key: null,
  on_air: null,
};

type InspectorMessage = AtemStateMessage | OptionsMessage;

type NumberField = "me" | "video_source" | "aux_bus" | "key";

export function App() {
  const deck = useStreamDeck();
  const action = deck.actionInfo?.action ?? "";
  const { settings, setSettings } = useSettings<ActionSettings>(EMPTY_SETTINGS);
  const sendToPlugin = useSendToPlugin();
  const [connected, setConnected] = useState(false);
  const [options, setOptions] = useState<Record<string, SelectItem[]>>({});

  usePluginMessage<InspectorMessage>((message) => {
    if (message.type === "atemState") {
      setConnected(message.connected);
    }
    if (message.type === "options" && "items" in message) {
      setConnected(message.connected);
      setOptions((previous) => ({ ...previous, [message.kind]: message.items }));
    }
  });

  useEffect(() => {
    const timer = window.setTimeout(() => {
      for (const kind of kindsFor(action)) {
        sendToPlugin({ type: "requestOptions", kind, atem_ip: settings.atem_ip });
      }
    }, 200);
    return () => window.clearTimeout(timer);
  }, [action, sendToPlugin, settings.atem_ip]);

  const setText = (atemIp: string) => {
    setSettings((previous) => ({ ...previous, atem_ip: atemIp }));
  };

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

  return (
    <div className="sdpi-wrapper">
      <div className="sdpi-heading">ATEM</div>
      <div className="sdpi-item">
        <div className="sdpi-item-label">ATEM IP address</div>
        <input
          className="sdpi-item-value"
          type="text"
          placeholder="192.168.1.100"
          value={settings.atem_ip ?? ""}
          onChange={(event) => setText(event.target.value)}
        />
      </div>

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

      <div className="sdpi-item">
        <div className="sdpi-item-label">Connection</div>
        <div className={`sdpi-item-value ${connected ? "status-connected" : "status-disconnected"}`}>
          {connected ? "Connected to ATEM" : "Not connected"}
        </div>
      </div>
    </div>
  );
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
