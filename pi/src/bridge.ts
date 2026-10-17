import type { GlobalSettings } from "./generated/contracts";

export type Status =
  | { kind: "disabled" }
  | { kind: "connecting" }
  | { kind: "connected" }
  | { kind: "disconnected"; message?: string }
  | { kind: string; message?: string };

export type LiveInstance = {
  id: string;
  name?: string;
  host?: string;
  port?: number;
  color?: string;
  enabled?: boolean;
  status?: Status;
};

export type AtemConfigBridge = {
  getSettings: () => GlobalSettings;
  setSettings: (settings: GlobalSettings) => void;
  reconnect: (id?: string) => void;
  subscribe: (listener: (instances: LiveInstance[]) => void) => () => void;
};

declare global {
  interface Window {
    atemConfig?: AtemConfigBridge;
    refreshAtemConfig?: (settings: GlobalSettings, instances: LiveInstance[]) => void;
  }
}
