export type Peer = {
  id: string;
  name: string;
  addr: string;
  fingerprint: string;
  paired: boolean;
  online: boolean;
  os: string;
  code: string | null;
};

export type Monitor = {
  id: string;
  device_id: string;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  scale: number;
};

export type Transfer = {
  id: number;
  label: string;
  bytes_done: number;
  bytes_total: number;
  bytes_per_sec: number;
  state: string;
  path: string;
};

export type Hotkeys = {
  next_monitor: string;
  prev_monitor: string;
  monitor_n: string;
  center: string;
  find_cursor: string;
  lock_monitor: string;
  jump_device_n: string;
  return_home: string;
  panic: string;
};

export type Snapshot = {
  device_id: string;
  device_name: string;
  paused: boolean;
  locked: boolean;
  remote: string | null;
  hop_mode: string;
  wrap: boolean;
  clipboard_sync: boolean;
  modifiers: { ctrl_cmd: boolean; win_option: boolean };
  scroll: { natural: boolean; speed: number };
  hotkeys: Hotkeys;
  peers: Peer[];
  monitors: Monitor[];
  transfers: Transfer[];
  pairing_code: string | null;
  local_code: string;
  local_addr: string;
  accessibility: boolean;
  input_monitoring: boolean;
  status: string;
  port: number;
};

export type Cmd =
  | { SetPaused: boolean }
  | { SetHopMode: string }
  | { SetWrap: boolean }
  | { SetHotkey: { action: string; chord: string } }
  | { SetClipboard: boolean }
  | { SetModifiers: { ctrl_cmd: boolean; win_option: boolean } }
  | { SetScroll: { natural: boolean; speed: number } }
  | { ConfirmPair: { id: string; code: string } }
  | { RejectPair: string }
  | { Unpair: string }
  | { AddIp: string }
  | { MoveMonitor: { id: string; x: number; y: number } }
  | { CancelTransfer: number }
  | { OpenTransfer: number }
  | "PromptPermissions";

let mock: Snapshot = {
  device_id: "local-1",
  device_name: "This PC",
  paused: false,
  locked: false,
  remote: null,
  hop_mode: "Relative",
  wrap: false,
  clipboard_sync: true,
  modifiers: { ctrl_cmd: true, win_option: true },
  scroll: { natural: false, speed: 1 },
  hotkeys: {
    next_monitor: "ctrl+alt+right",
    prev_monitor: "ctrl+alt+left",
    monitor_n: "ctrl+alt+{n}",
    center: "ctrl+alt+c",
    find_cursor: "ctrl+alt+f",
    lock_monitor: "scrolllock",
    jump_device_n: "ctrl+alt+shift+{n}",
    return_home: "ctrl+alt+shift+h",
    panic: "ctrl+alt+esc",
  },
  peers: [
    {
      id: "peer-mac",
      name: "MacBook",
      addr: "192.168.1.20:42424",
      fingerprint: "ab".repeat(32),
      paired: false,
      online: true,
      os: "Macos",
      code: "482913",
    },
  ],
  monitors: [
    { id: "local-1:1", device_id: "local-1", name: "\\\\.\\DISPLAY1", x: -1920, y: 0, width: 1920, height: 1080, scale: 1 },
    { id: "local-1:2", device_id: "local-1", name: "\\\\.\\DISPLAY2", x: 0, y: 0, width: 1920, height: 1080, scale: 1 },
    { id: "peer-mac:1", device_id: "peer-mac", name: "Mac", x: 4480, y: 0, width: 1512, height: 982, scale: 2 },
  ],
  transfers: [
    {
      id: 1,
      label: "notes",
      bytes_done: 12_000_000,
      bytes_total: 40_000_000,
      bytes_per_sec: 8_000_000,
      state: "active",
      path: "Downloads/DevHop",
    },
  ],
  pairing_code: null,
  local_code: "159264",
  local_addr: "",
  accessibility: true,
  input_monitoring: true,
  status: "ready",
  port: 42424,
};

const listeners = new Set<() => void>();

function inTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function publish() {
  for (const listener of listeners) listener();
}

export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export async function loadSnapshot(): Promise<Snapshot> {
  if (!inTauri()) return structuredClone(mock);
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<Snapshot>("snapshot");
}

export async function send(cmd: Cmd): Promise<void> {
  if (!inTauri()) {
    applyMock(cmd);
    publish();
    return;
  }
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("command", { cmd });
}

export async function listenFind(onFind: (x: number, y: number) => void): Promise<() => void> {
  if (!inTauri()) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  const unlisten = await listen<{ x: number; y: number }>("find-cursor", (event) => {
    onFind(event.payload.x, event.payload.y);
  });
  return unlisten;
}

export async function setAutostart(enabled: boolean): Promise<void> {
  if (!inTauri()) {
    mock.status = enabled ? "autostart on" : "autostart off";
    publish();
    return;
  }
  const { enable, disable } = await import("@tauri-apps/plugin-autostart");
  if (enabled) await enable();
  else await disable();
}

export async function showFolder(path: string): Promise<void> {
  if (!inTauri()) {
    mock.status = `open ${path}`;
    publish();
    return;
  }
  const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
  await revealItemInDir(path);
}

function applyMock(cmd: Cmd) {
  if (cmd === "PromptPermissions") {
    mock.accessibility = true;
    mock.input_monitoring = true;
    mock.status = "permissions granted";
    return;
  }
  if ("SetPaused" in cmd) mock.paused = cmd.SetPaused;
  else if ("SetHopMode" in cmd) mock.hop_mode = cmd.SetHopMode;
  else if ("SetWrap" in cmd) mock.wrap = cmd.SetWrap;
  else if ("SetClipboard" in cmd) mock.clipboard_sync = cmd.SetClipboard;
  else if ("SetModifiers" in cmd) mock.modifiers = cmd.SetModifiers;
  else if ("SetScroll" in cmd) mock.scroll = cmd.SetScroll;
  else if ("SetHotkey" in cmd) {
    const key = cmd.SetHotkey.action as keyof Hotkeys;
    if (key in mock.hotkeys) mock.hotkeys[key] = cmd.SetHotkey.chord;
  } else if ("ConfirmPair" in cmd) {
    const { id, code } = cmd.ConfirmPair;
    const peer = mock.peers.find((item) => item.id === id);
    if (!peer || peer.code !== code) {
      mock.status = "pairing code does not match";
      return;
    }
    if (peer) {
      peer.paired = true;
      peer.code = null;
    }
    mock.pairing_code = null;
    mock.status = "paired";
  } else if ("RejectPair" in cmd) {
    const peer = mock.peers.find((item) => item.id === cmd.RejectPair);
    if (peer) peer.code = null;
    mock.pairing_code = null;
    mock.status = "pairing rejected";
  } else if ("Unpair" in cmd) {
    const peer = mock.peers.find((item) => item.id === cmd.Unpair);
    if (peer) {
      peer.paired = false;
      peer.online = false;
    }
  } else if ("AddIp" in cmd) {
    mock.peers.push({
      id: `ip-${mock.peers.length}`,
      name: cmd.AddIp,
      addr: cmd.AddIp,
      fingerprint: "",
      paired: false,
      online: false,
      os: "",
      code: null,
    });
  } else if ("MoveMonitor" in cmd) {
    const monitor = mock.monitors.find((item) => item.id === cmd.MoveMonitor.id);
    if (monitor) {
      monitor.x = cmd.MoveMonitor.x;
      monitor.y = cmd.MoveMonitor.y;
    }
  } else if ("CancelTransfer" in cmd) {
    const transfer = mock.transfers.find((item) => item.id === cmd.CancelTransfer);
    if (transfer) transfer.state = "cancel";
  } else if ("OpenTransfer" in cmd) {
    const transfer = mock.transfers.find((item) => item.id === cmd.OpenTransfer);
    mock.status = transfer ? `open ${transfer.path}` : mock.status;
  }
}
