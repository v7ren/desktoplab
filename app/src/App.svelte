<script lang="ts">
  import { onMount } from "svelte";
  import { loadSnapshot, listenFind, send, setAutostart, showFolder, subscribe, type Snapshot } from "./lib/api";

  const pages = ["Layout", "Hotkeys", "Devices", "Clipboard", "Transfers", "About"] as const;
  type Page = (typeof pages)[number];

  let page = $state<Page>("Layout");
  let snap = $state<Snapshot | null>(null);
  let ip = $state("");
  let find = $state<{ x: number; y: number } | null>(null);
  let autostart = $state(false);
  let error = $state("");

  const hotkeyFields = [
    ["next_monitor", "Next monitor"],
    ["prev_monitor", "Previous monitor"],
    ["monitor_n", "Monitor number"],
    ["center", "Center"],
    ["find_cursor", "Find cursor"],
    ["lock_monitor", "Lock monitor"],
    ["jump_device_n", "Jump to device"],
    ["return_home", "Return home"],
    ["panic", "Panic"],
  ] as const;

  async function refresh() {
    try {
      snap = await loadSnapshot();
      error = "";
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    }
  }

  onMount(() => {
    void refresh();
    const stop = subscribe(() => {
      void refresh();
    });
    const timer = setInterval(() => {
      void refresh();
    }, 1000);
    let unlisten = () => {};
    void listenFind((x, y) => {
      find = { x, y };
      setTimeout(() => {
        find = null;
      }, 1600);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      stop();
      clearInterval(timer);
      unlisten();
    };
  });

  function scale(value: number): number {
    return value / 12;
  }

  function monitorBox(monitor: Snapshot["monitors"][number], monitors: Snapshot["monitors"]) {
    const minX = Math.min(...monitors.map((item) => item.x));
    const minY = Math.min(...monitors.map((item) => item.y));
    const pad = 16;
    return {
      left: scale(monitor.x - minX) + pad,
      top: scale(monitor.y - minY) + pad,
      width: Math.max(scale(monitor.width), 80),
      height: Math.max(scale(monitor.height), 48),
    };
  }

  function monitorLabel(name: string): string {
    const slash = name.lastIndexOf("\\");
    return slash >= 0 ? name.slice(slash + 1) : name;
  }

  function onMonitorPointer(event: PointerEvent, id: string) {
    const target = event.currentTarget as HTMLElement;
    target.setPointerCapture(event.pointerId);
    const startX = event.clientX;
    const startY = event.clientY;
    const monitor = snap?.monitors.find((item) => item.id === id);
    if (!monitor) return;
    const originX = monitor.x;
    const originY = monitor.y;
    const move = (ev: PointerEvent) => {
      const x = Math.round(originX + (ev.clientX - startX) * 12);
      const y = Math.round(originY + (ev.clientY - startY) * 12);
      void send({ MoveMonitor: { id, x, y } }).then(refresh);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }
</script>

<main>
  <aside>
    <h1>DevHop</h1>
    <p class="muted">{snap?.device_name ?? "…"}</p>
    <nav>
      {#each pages as name}
        <button class:active={page === name} onclick={() => (page = name)}>{name}</button>
      {/each}
    </nav>
    <p class="status">{snap?.status ?? ""}</p>
  </aside>
  <section>
    {#if error}
      <p class="error">{error}</p>
    {/if}
    {#if snap && (!snap.accessibility || !snap.input_monitoring)}
      <div class="card">
        <h2>Permissions</h2>
        <p>Accessibility and Input Monitoring have to be on before DevHop can move the cursor.</p>
        <button onclick={() => send("PromptPermissions").then(refresh)}>Grant permissions</button>
      </div>
    {/if}
    {#if snap?.pairing_code}
      <div class="card pair">
        <h2>Pairing code</h2>
        <p class="code">{snap.pairing_code}</p>
        <p>Confirm this number matches the other computer.</p>
      </div>
    {/if}
    {#if page === "Layout" && snap}
      <h2>Layout</h2>
      <label>Hop mode
        <select value={snap.hop_mode} onchange={(e) => send({ SetHopMode: e.currentTarget.value }).then(refresh)}>
          <option>Relative</option>
          <option>Memory</option>
          <option>Center</option>
        </select>
      </label>
      <label class="check"><input type="checkbox" checked={snap.wrap} onchange={(e) => send({ SetWrap: e.currentTarget.checked }).then(refresh)} /> Wrap around this computer</label>
      <label class="check"><input type="checkbox" checked={snap.paused} onchange={(e) => send({ SetPaused: e.currentTarget.checked }).then(refresh)} /> Pause crossing</label>
      <div class="canvas" aria-label="Monitor layout">
        {#each snap.monitors as monitor}
          {@const box = monitorBox(monitor, snap.monitors)}
          <button
            class="monitor"
            class:remote={monitor.device_id !== snap.device_id}
            style="left:{box.left}px;top:{box.top}px;width:{box.width}px;height:{box.height}px"
            onpointerdown={(event) => onMonitorPointer(event, monitor.id)}
          >
            {monitorLabel(monitor.name)}
          </button>
        {/each}
      </div>
    {:else if page === "Hotkeys" && snap}
      <h2>Hotkeys</h2>
      {#each hotkeyFields as [action, label]}
        <label>{label}
          <input
            value={snap.hotkeys[action]}
            onchange={(e) => send({ SetHotkey: { action, chord: e.currentTarget.value } }).then(refresh)}
          />
        </label>
      {/each}
    {:else if page === "Devices" && snap}
      <h2>Devices</h2>
      <form onsubmit={(e) => { e.preventDefault(); void send({ AddIp: ip }).then(() => { ip = ""; return refresh(); }); }}>
        <input bind:value={ip} placeholder="192.168.1.20" aria-label="Manual address" />
        <button type="submit">Add by IP</button>
      </form>
      <ul>
        {#each snap.peers as peer}
          <li>
            <strong>{peer.name}</strong>
            <span class="muted">{peer.addr} {peer.os} {peer.online ? "online" : "offline"}</span>
            {#if peer.code}
              <span class="code">{peer.code}</span>
              <button onclick={() => send({ ConfirmPair: peer.id }).then(refresh)}>Confirm</button>
              <button onclick={() => send({ RejectPair: peer.id }).then(refresh)}>Reject</button>
            {:else if peer.paired}
              <button onclick={() => send({ Unpair: peer.id }).then(refresh)}>Unpair</button>
            {/if}
          </li>
        {/each}
      </ul>
    {:else if page === "Clipboard" && snap}
      <h2>Clipboard</h2>
      <label class="check"><input type="checkbox" checked={snap.clipboard_sync} onchange={(e) => send({ SetClipboard: e.currentTarget.checked }).then(refresh)} /> Sync clipboard on crossing</label>
      <label class="check"><input type="checkbox" checked={snap.modifiers.ctrl_cmd} onchange={(e) => send({ SetModifiers: { ...snap.modifiers, ctrl_cmd: e.currentTarget.checked } }).then(refresh)} /> Windows Ctrl ↔ Mac Command</label>
      <label class="check"><input type="checkbox" checked={snap.modifiers.win_option} onchange={(e) => send({ SetModifiers: { ...snap.modifiers, win_option: e.currentTarget.checked } }).then(refresh)} /> Windows Win ↔ Mac Option</label>
      <label class="check"><input type="checkbox" checked={snap.scroll.natural} onchange={(e) => send({ SetScroll: { ...snap.scroll, natural: e.currentTarget.checked } }).then(refresh)} /> Natural scrolling</label>
      <label>Scroll speed
        <input type="number" min="0.1" step="0.1" value={snap.scroll.speed} onchange={(e) => send({ SetScroll: { ...snap.scroll, speed: Number(e.currentTarget.value) } }).then(refresh)} />
      </label>
    {:else if page === "Transfers" && snap}
      <h2>Transfers</h2>
      {#if snap.transfers.length === 0}
        <p class="muted">No transfers yet.</p>
      {/if}
      <ul>
        {#each snap.transfers as transfer}
          <li>
            <strong>{transfer.label}</strong>
            <progress max={transfer.bytes_total || 1} value={transfer.bytes_done}></progress>
            <span class="muted">{transfer.state}</span>
            <button onclick={() => send({ CancelTransfer: transfer.id }).then(refresh)}>Cancel</button>
            <button onclick={() => { void send({ OpenTransfer: transfer.id }); void showFolder(transfer.path); }}>Show in folder</button>
          </li>
        {/each}
      </ul>
    {:else if page === "About" && snap}
      <h2>About</h2>
      <p>DevHop {snap.device_id}</p>
      <p class="muted">Port {snap.port}. Locked: {snap.locked ? "yes" : "no"}. Remote: {snap.remote ?? "local"}.</p>
      <label class="check"><input type="checkbox" checked={autostart} onchange={(e) => { autostart = e.currentTarget.checked; void setAutostart(autostart); }} /> Start with the computer</label>
    {/if}
  </section>
  {#if find}
    <div class="ring" style="left:{find.x}px;top:{find.y}px"></div>
  {/if}
</main>

<style>
  :global(body) {
    margin: 0;
    font-family: "Segoe UI", sans-serif;
    background: #101114;
    color: #f2f2f2;
  }
  main { display: grid; grid-template-columns: 200px 1fr; min-height: 100vh; }
  aside { padding: 20px; background: #181a1f; border-right: 1px solid #2a2d34; }
  h1 { margin: 0 0 4px; font-size: 22px; }
  nav { display: flex; flex-direction: column; gap: 6px; margin-top: 20px; }
  button, select, input { font: inherit; }
  nav button, button {
    background: #2a2d34;
    color: inherit;
    border: 0;
    border-radius: 8px;
    padding: 8px 10px;
    text-align: left;
    cursor: pointer;
  }
  nav button.active { background: #3d6df2; }
  section { padding: 24px 28px; }
  label { display: flex; flex-direction: column; gap: 6px; margin: 12px 0; max-width: 360px; }
  label.check { flex-direction: row; align-items: center; }
  input, select { background: #181a1f; color: inherit; border: 1px solid #3a3d46; border-radius: 8px; padding: 8px; }
  .muted { color: #9aa0ab; }
  .status { margin-top: 24px; color: #9aa0ab; font-size: 13px; }
  .error { color: #ff8d8d; }
  .card { background: #181a1f; padding: 16px; border-radius: 12px; margin-bottom: 16px; }
  .code { font-size: 32px; letter-spacing: 6px; margin: 0; }
  .canvas { position: relative; height: 420px; background: #181a1f; border-radius: 12px; overflow: auto; }
  .monitor {
    position: absolute;
    background: #243056;
    border: 1px solid #6d84d6;
    border-radius: 6px;
  }
  .monitor.remote { background: #243828; border-color: #6db57a; }
  ul { list-style: none; padding: 0; }
  li { display: flex; gap: 10px; align-items: center; padding: 10px 0; border-bottom: 1px solid #2a2d34; flex-wrap: wrap; }
  progress { width: 140px; }
  .ring {
    position: fixed;
    width: 80px;
    height: 80px;
    margin: -40px 0 0 -40px;
    border: 4px solid #7aa2ff;
    border-radius: 50%;
    pointer-events: none;
  }
</style>
