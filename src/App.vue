<script setup lang="ts">
import { ref, onMounted, onUnmounted } from "vue";
import { getCurrentWindow, currentMonitor } from "@tauri-apps/api/window";
import { PhysicalPosition } from "@tauri-apps/api/dpi";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface ScorePayload {
  total_score: number;
  session_score: number;
  selection_count: number;
  name_count: number;
  created_count: number;
  state: "Idle" | "Scanning" | "Connected";
  recent_events: number;
}

const appWindow = getCurrentWindow();
const pinned = ref(true);
const snapped = ref(false);

// Score / connection state
const totalScore = ref(0);
const sessionScore = ref(0);
const connState = ref<"Idle" | "Scanning" | "Connected">("Idle");
const recentEvents = ref(0);

const SNAP_PX = 24; // 距边缘多少像素内触发吸附
let snapTimer: ReturnType<typeof setTimeout> | null = null;
let unlistenMoved: (() => void) | null = null;
let unlistenScore: UnlistenFn | null = null;

// #region agent log
let dbgPayloadIngress = 0;
// #endregion

function applyPayload(p: ScorePayload) {
  // #region agent log
  if (dbgPayloadIngress < 28) {
    dbgPayloadIngress++;
    fetch("http://127.0.0.1:7655/ingest/b745c231-98ac-4235-84c6-91081f552b83", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Debug-Session-Id": "9347d3",
      },
      body: JSON.stringify({
        sessionId: "9347d3",
        location: "App.vue:applyPayload",
        message: "score_payload_applied",
        data: { state: p.state, session_score: p.session_score },
        timestamp: Date.now(),
        hypothesisId: "H5",
        runId: "post-fix-2",
      }),
    }).catch(() => {});
  }
  // #endregion
  totalScore.value = p.total_score;
  sessionScore.value = p.session_score;
  connState.value = p.state;
  recentEvents.value = p.recent_events;
}

function startDrag(e: MouseEvent) {
  if (e.button === 0) {
    appWindow.startDragging();
  }
}

function closeWindow() {
  appWindow.close();
}

async function togglePin() {
  pinned.value = !pinned.value;
  await appWindow.setAlwaysOnTop(pinned.value);
}

async function snapToEdge() {
  const [pos, monitor, size] = await Promise.all([
    appWindow.outerPosition(),
    currentMonitor(),
    appWindow.outerSize(),
  ]);
  if (!monitor) return;

  const mx = monitor.position.x;
  const my = monitor.position.y;
  const mw = monitor.size.width;
  const mh = monitor.size.height;
  const ww = size.width;
  const wh = size.height;

  let nx = pos.x;
  let ny = pos.y;

  if (pos.x - mx < SNAP_PX) nx = mx;
  else if (mx + mw - (pos.x + ww) < SNAP_PX) nx = mx + mw - ww;

  if (pos.y - my < SNAP_PX) ny = my;
  else if (my + mh - (pos.y + wh) < SNAP_PX) ny = my + mh - wh;

  if (nx !== pos.x || ny !== pos.y) {
    await appWindow.setPosition(new PhysicalPosition(nx, ny));
    snapped.value = true;
    setTimeout(() => (snapped.value = false), 400);
  }
}

onMounted(async () => {
  unlistenMoved = await appWindow.onMoved(() => {
    if (snapTimer) clearTimeout(snapTimer);
    snapTimer = setTimeout(snapToEdge, 120);
  });

  // Load initial score state immediately on mount
  try {
    const initial = await invoke<ScorePayload>("get_score");
    applyPayload(initial);
  } catch (_) {
    // backend not ready yet — will arrive via event
  }

  // Subscribe to live updates
  unlistenScore = await listen<ScorePayload>("score-updated", (e) => {
    applyPayload(e.payload);
  });
});

onUnmounted(() => {
  unlistenMoved?.();
  unlistenScore?.();
  if (snapTimer) clearTimeout(snapTimer);
});
</script>

<template>
  <div class="widget" :class="{ snapped }" @contextmenu.prevent>
    <!-- 拖拽栏 -->
    <header class="bar" @mousedown="startDrag">
      <div class="bar-left">
        <span class="pip" :class="{ lit: pinned }"></span>
        <span class="bar-name">WWISE · RANK</span>
      </div>
      <div class="bar-right">
        <button
          class="ico-btn"
          :class="{ 'ico-on': pinned }"
          title="置顶"
          @mousedown.stop
          @click="togglePin"
        >
          <svg width="11" height="11" viewBox="0 0 11 11" fill="none">
            <path d="M5.5 1.5v5M3.5 5l2 1.5 2-1.5M2.5 9h6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>
          </svg>
        </button>
        <button class="ico-btn ico-close" title="关闭" @mousedown.stop @click="closeWindow">
          <svg width="9" height="9" viewBox="0 0 9 9" fill="none">
            <path d="M1.5 1.5l6 6M7.5 1.5l-6 6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
          </svg>
        </button>
      </div>
    </header>

    <!-- 主体 -->
    <main class="body">
      <!-- 三栏读数 -->
      <div class="readouts">
        <div class="readout">
          <div class="val">{{ totalScore }}</div>
          <div class="key">RANK</div>
        </div>
        <div class="r-sep"></div>
        <div class="readout">
          <div class="val">{{ sessionScore }}</div>
          <div class="key">SCORE</div>
        </div>
        <div class="r-sep"></div>
        <div class="readout">
          <div class="val state-val" :class="connState.toLowerCase()">{{ connState.toUpperCase() }}</div>
          <div class="key">STATE</div>
        </div>
      </div>

      <!-- VU 电平条 (活动指示器) -->
      <div class="vu-row">
        <span class="vu-lbl">ACT</span>
        <div class="vu-meter">
          <div v-for="i in 24" :key="i" class="seg" :class="{ on: i <= recentEvents }"></div>
        </div>
        <span class="vu-db">{{ recentEvents > 0 ? recentEvents : '–' }}</span>
      </div>

      <!-- 操作按钮 -->
      <div class="actions">
        <button class="btn-fill">ANALYZE</button>
        <button class="btn-ghost">SETTINGS</button>
      </div>
    </main>
  </div>
</template>

<style>
@import url('https://fonts.googleapis.com/css2?family=Barlow+Condensed:wght@400;600;700&family=Barlow:wght@400;500&display=swap');

*, *::before, *::after { margin: 0; padding: 0; box-sizing: border-box; }

html, body, #app {
  width: 100%;
  height: 100%;
  background: transparent;
  overflow: hidden;
  -webkit-font-smoothing: antialiased;
  user-select: none;
}
</style>

<style scoped>
/* ── 设计令牌 ─────────────────────────────── */
.widget {
  --bg:      oklch(13% 0.010 255);
  --bg-bar:  oklch(10% 0.012 255);
  --border:  oklch(22% 0.015 255);
  --accent:  oklch(70% 0.17 63);
  --a-dim:   oklch(50% 0.13 63);
  --a-glow:  oklch(70% 0.17 63 / 25%);
  --text:    oklch(87% 0.010 80);
  --muted:   oklch(42% 0.012 255);
  --green:   oklch(67% 0.19 150);
  --seg-off: oklch(22% 0.018 255);
}

/* ── 外壳 ──────────────────────────────────── */
.widget {
  width: 320px;
  height: 200px;
  border-radius: 4px;
  background: var(--bg);
  border: 1px solid var(--border);
  border-top: 2px solid var(--accent);
  box-shadow:
    0 0 0 1px oklch(0% 0 0 / 60%),
    0 12px 40px oklch(0% 0 0 / 55%),
    0 0 20px var(--a-glow);
  display: flex;
  flex-direction: column;
  overflow: hidden;
  font-family: 'Barlow Condensed', 'Barlow', 'Segoe UI', sans-serif;
  color: var(--text);
  animation: mount 0.25s cubic-bezier(0.22, 1, 0.36, 1) both;
}

@keyframes mount {
  from { opacity: 0; transform: scale(0.96) translateY(4px); }
  to   { opacity: 1; transform: scale(1) translateY(0); }
}

.widget.snapped {
  border-top-color: oklch(80% 0.18 63);
  box-shadow:
    0 0 0 1px oklch(0% 0 0 / 60%),
    0 12px 40px oklch(0% 0 0 / 55%),
    0 0 28px oklch(70% 0.17 63 / 45%);
  animation: snap-flash 0.4s cubic-bezier(0.22, 1, 0.36, 1);
}

@keyframes snap-flash {
  0%   { box-shadow: 0 0 0 1px oklch(0% 0 0 / 60%), 0 0 0 oklch(70% 0.17 63 / 0%); }
  40%  { box-shadow: 0 0 0 1px oklch(0% 0 0 / 60%), 0 0 36px oklch(70% 0.17 63 / 55%); }
  100% { box-shadow: 0 0 0 1px oklch(0% 0 0 / 60%), 0 12px 40px oklch(0% 0 0 / 55%), 0 0 20px oklch(70% 0.17 63 / 25%); }
}

/* ── 标题栏 ─────────────────────────────────── */
.bar {
  height: 30px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 10px;
  background: var(--bg-bar);
  border-bottom: 1px solid var(--border);
  cursor: grab;
  flex-shrink: 0;
}
.bar:active { cursor: grabbing; }

.bar-left {
  display: flex;
  align-items: center;
  gap: 7px;
}

.pip {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--muted);
  transition: background 0.2s, box-shadow 0.2s;
  flex-shrink: 0;
}
.pip.lit {
  background: var(--accent);
  box-shadow: 0 0 6px var(--accent);
}

.bar-name {
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 1.8px;
  color: var(--muted);
  text-transform: uppercase;
}

.bar-right {
  display: flex;
  align-items: center;
  gap: 4px;
}

.ico-btn {
  width: 22px;
  height: 22px;
  display: flex;
  align-items: center;
  justify-content: center;
  background: none;
  border: 1px solid transparent;
  border-radius: 3px;
  color: var(--muted);
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s, background 0.15s;
}
.ico-btn:hover {
  color: var(--text);
  border-color: var(--border);
  background: oklch(100% 0 0 / 4%);
}
.ico-btn.ico-on {
  color: var(--accent);
  border-color: var(--a-dim);
  background: var(--a-glow);
}
.ico-btn.ico-close:hover {
  color: oklch(70% 0.2 20);
  border-color: oklch(45% 0.2 20);
  background: oklch(70% 0.2 20 / 12%);
}

/* ── 主体 ───────────────────────────────────── */
.body {
  flex: 1;
  display: flex;
  flex-direction: column;
  padding: 12px 14px 10px;
  gap: 10px;
}

/* ── 读数 ───────────────────────────────────── */
.readouts {
  display: flex;
  align-items: stretch;
  gap: 0;
  flex: 1;
}

.readout {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 3px;
  padding: 6px 4px;
  border-radius: 2px;
  background: oklch(100% 0 0 / 2%);
}

.val {
  font-size: 24px;
  font-weight: 700;
  line-height: 1;
  color: var(--accent);
  letter-spacing: -0.5px;
  font-feature-settings: "tnum";
}

.state-val {
  font-size: 13px;
  letter-spacing: 1px;
}
.state-val.idle {
  color: var(--muted);
}
.state-val.scanning {
  color: var(--accent);
  animation: pulse 1s ease-in-out infinite;
}
.state-val.connected {
  color: var(--green);
}

@keyframes pulse {
  0%, 100% { opacity: 1; }
  50%       { opacity: 0.4; }
}

.key {
  font-size: 9px;
  font-weight: 600;
  letter-spacing: 1.5px;
  color: var(--muted);
  text-transform: uppercase;
}

.r-sep {
  width: 1px;
  background: var(--border);
  margin: 6px 0;
  align-self: stretch;
}

/* ── VU 电平条 ─────────────────────────────── */
.vu-row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.vu-lbl, .vu-db {
  font-size: 9px;
  font-weight: 600;
  letter-spacing: 1px;
  color: var(--muted);
  flex-shrink: 0;
  width: 22px;
}
.vu-db { text-align: right; }

.vu-meter {
  flex: 1;
  display: flex;
  gap: 2px;
  align-items: center;
}

.seg {
  flex: 1;
  height: 8px;
  border-radius: 1px;
  background: var(--seg-off);
  transition: background 0.15s;
}
.seg.on {
  background: var(--accent);
}

/* ── 操作按钮 ──────────────────────────────── */
.actions {
  display: flex;
  gap: 8px;
}

.btn-fill, .btn-ghost {
  flex: 1;
  height: 28px;
  border-radius: 2px;
  font-family: inherit;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 1.5px;
  cursor: pointer;
  transition: all 0.15s;
}

.btn-fill {
  background: var(--accent);
  border: 1px solid var(--accent);
  color: oklch(10% 0.01 255);
}
.btn-fill:hover {
  background: oklch(75% 0.18 63);
  border-color: oklch(75% 0.18 63);
  box-shadow: 0 0 12px var(--a-glow);
}
.btn-fill:active {
  background: oklch(64% 0.16 63);
}

.btn-ghost {
  background: transparent;
  border: 1px solid var(--border);
  color: var(--muted);
}
.btn-ghost:hover {
  border-color: oklch(35% 0.02 255);
  color: var(--text);
  background: oklch(100% 0 0 / 3%);
}
.btn-ghost:active {
  background: oklch(100% 0 0 / 6%);
}
</style>
