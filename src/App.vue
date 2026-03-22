<script setup lang="ts">
import { ref, onMounted, onUnmounted } from "vue";
import { getCurrentWindow, currentMonitor } from "@tauri-apps/api/window";
import { PhysicalPosition } from "@tauri-apps/api/dpi";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface ScorePayload {
  total_score: number;
  session_score: number;
  event_counts: Record<string, number>;
  state: "Idle" | "Scanning" | "Connected";
  recent_events: number;
}

const appWindow = getCurrentWindow();
const pinned = ref(true);
const snapped = ref(false);

// Score / connection state (backing values)
const totalScore = ref(0);
const sessionScore = ref(0);
const connState = ref<"Idle" | "Scanning" | "Connected">("Idle");
const recentEvents = ref(0);

// Animated display values
const displayTotal = ref(0);
const displaySession = ref(0);

// Flash states for change feedback
const rankFlash = ref(false);
const scoreFlash = ref(false);
const stateFlash = ref(false);

const SNAP_PX = 24;
let snapTimer: ReturnType<typeof setTimeout> | null = null;
let unlistenMoved: (() => void) | null = null;
let unlistenScore: UnlistenFn | null = null;

// Eased number interpolation (ease-out cubic)
function animateTo(
  displayRef: { value: number },
  to: number,
  frameIdHolder: { id: number | null }
) {
  if (frameIdHolder.id !== null) cancelAnimationFrame(frameIdHolder.id);
  const from = displayRef.value;
  if (from === to) return;
  const duration = 380;
  const start = performance.now();
  function step(now: number) {
    const t = Math.min((now - start) / duration, 1);
    const eased = 1 - (1 - t) ** 3; // ease-out cubic
    displayRef.value = Math.round(from + (to - from) * eased);
    if (t < 1) {
      frameIdHolder.id = requestAnimationFrame(step);
    } else {
      frameIdHolder.id = null;
    }
  }
  frameIdHolder.id = requestAnimationFrame(step);
}

const totalFrameHolder = { id: null as number | null };
const sessionFrameHolder = { id: null as number | null };

function triggerFlash(
  flashRef: { value: boolean },
  timerHolder: { timer: ReturnType<typeof setTimeout> | null },
  duration = 500
) {
  if (timerHolder.timer) clearTimeout(timerHolder.timer);
  flashRef.value = true;
  timerHolder.timer = setTimeout(() => {
    flashRef.value = false;
    timerHolder.timer = null;
  }, duration);
}

const rankFlashHolder = { timer: null as ReturnType<typeof setTimeout> | null };
const scoreFlashHolder = { timer: null as ReturnType<typeof setTimeout> | null };
const stateFlashHolder = { timer: null as ReturnType<typeof setTimeout> | null };

function applyPayload(p: ScorePayload) {
  if (p.total_score !== totalScore.value) {
    totalScore.value = p.total_score;
    animateTo(displayTotal, p.total_score, totalFrameHolder);
    triggerFlash(rankFlash, rankFlashHolder);
  }
  if (p.session_score !== sessionScore.value) {
    sessionScore.value = p.session_score;
    animateTo(displaySession, p.session_score, sessionFrameHolder);
    triggerFlash(scoreFlash, scoreFlashHolder);
  }
  if (p.state !== connState.value) {
    connState.value = p.state;
    triggerFlash(stateFlash, stateFlashHolder, 600);
  }
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

// Button ripple effect
function createRipple(e: MouseEvent) {
  const button = e.currentTarget as HTMLElement;
  const ripple = document.createElement("span");
  const rect = button.getBoundingClientRect();
  ripple.className = "ripple";
  ripple.style.left = e.clientX - rect.left + "px";
  ripple.style.top = e.clientY - rect.top + "px";
  button.appendChild(ripple);
  setTimeout(() => ripple.remove(), 650);
}

onMounted(async () => {
  unlistenMoved = await appWindow.onMoved(() => {
    if (snapTimer) clearTimeout(snapTimer);
    snapTimer = setTimeout(snapToEdge, 120);
  });

  try {
    const initial = await invoke<ScorePayload>("get_score");
    applyPayload(initial);
    // Seed display values without animation on first load
    displayTotal.value = initial.total_score;
    displaySession.value = initial.session_score;
  } catch (_) {
    // backend not ready yet — will arrive via event
  }

  unlistenScore = await listen<ScorePayload>("score-updated", (e) => {
    applyPayload(e.payload);
  });
});

onUnmounted(() => {
  unlistenMoved?.();
  unlistenScore?.();
  if (snapTimer) clearTimeout(snapTimer);
  if (totalFrameHolder.id !== null) cancelAnimationFrame(totalFrameHolder.id);
  if (sessionFrameHolder.id !== null) cancelAnimationFrame(sessionFrameHolder.id);
  if (rankFlashHolder.timer) clearTimeout(rankFlashHolder.timer);
  if (scoreFlashHolder.timer) clearTimeout(scoreFlashHolder.timer);
  if (stateFlashHolder.timer) clearTimeout(stateFlashHolder.timer);
});

// Segment zone classification
function segClass(i: number, active: boolean) {
  if (!active) return "";
  if (i > 20) return "on danger";
  if (i > 16) return "on warn";
  return "on";
}
</script>

<template>
  <div
    class="widget"
    :class="{
      snapped,
      scanning: connState === 'Scanning',
      connected: connState === 'Connected',
    }"
    @contextmenu.prevent
  >
    <!-- 扫描线覆盖层 (仅 Scanning 状态) -->
    <div v-if="connState === 'Scanning'" class="scan-overlay" aria-hidden="true">
      <div class="scan-line"></div>
    </div>

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
            <path
              d="M5.5 1.5v5M3.5 5l2 1.5 2-1.5M2.5 9h6"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
        <button
          class="ico-btn ico-close"
          title="关闭"
          @mousedown.stop
          @click="closeWindow"
        >
          <svg width="9" height="9" viewBox="0 0 9 9" fill="none">
            <path
              d="M1.5 1.5l6 6M7.5 1.5l-6 6"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
            />
          </svg>
        </button>
      </div>
    </header>

    <!-- 主体 -->
    <main class="body">
      <!-- 三栏读数 -->
      <div class="readouts">
        <div class="readout">
          <div class="val" :class="{ flash: rankFlash }">{{ displayTotal }}</div>
          <div class="key">RANK</div>
        </div>
        <div class="r-sep"></div>
        <div class="readout">
          <div class="val" :class="{ flash: scoreFlash }">{{ displaySession }}</div>
          <div class="key">SCORE</div>
        </div>
        <div class="r-sep"></div>
        <div class="readout">
          <div
            class="val state-val"
            :class="[connState.toLowerCase(), { flash: stateFlash }]"
          >
            {{ connState.toUpperCase() }}
          </div>
          <div class="key">STATE</div>
        </div>
      </div>

      <!-- VU 电平条 -->
      <div class="vu-row">
        <span class="vu-lbl">ACT</span>
        <div class="vu-meter">
          <div
            v-for="i in 24"
            :key="i"
            class="seg"
            :class="segClass(i, i <= recentEvents)"
            :style="{ '--i': i }"
          ></div>
        </div>
        <span class="vu-db">{{ recentEvents > 0 ? recentEvents : "–" }}</span>
      </div>

      <!-- 操作按钮 -->
      <div class="actions">
        <button class="btn-fill" @click="createRipple">ANALYZE</button>
        <button class="btn-ghost" @click="createRipple">SETTINGS</button>
      </div>
    </main>
  </div>
</template>

<style>
@import url("https://fonts.googleapis.com/css2?family=Barlow+Condensed:wght@400;600;700&family=Barlow:wght@400;500&display=swap");

*,
*::before,
*::after {
  margin: 0;
  padding: 0;
  box-sizing: border-box;
}

html,
body,
#app {
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
  --bg: oklch(13% 0.01 255);
  --bg-bar: oklch(10% 0.012 255);
  --border: oklch(22% 0.015 255);
  --accent: oklch(70% 0.17 63);
  --a-dim: oklch(50% 0.13 63);
  --a-glow: oklch(70% 0.17 63 / 25%);
  --text: oklch(87% 0.01 80);
  --muted: oklch(42% 0.012 255);
  --green: oklch(67% 0.19 150);
  --green-glow: oklch(67% 0.19 150 / 30%);
  --seg-off: oklch(22% 0.018 255);
  --seg-warn: oklch(72% 0.19 45);
  --seg-danger: oklch(63% 0.24 22);
  --ease-out-quint: cubic-bezier(0.22, 1, 0.36, 1);
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
  font-family: "Barlow Condensed", "Barlow", "Segoe UI", sans-serif;
  color: var(--text);
  animation: mount 0.3s var(--ease-out-quint) both;
  position: relative;
}

@keyframes mount {
  from {
    opacity: 0;
    transform: scale(0.95) translateY(6px);
  }
  to {
    opacity: 1;
    transform: scale(1) translateY(0);
  }
}

/* Connected 状态 — 顶边绿色微光 */
.widget.connected {
  border-top-color: var(--green);
  box-shadow:
    0 0 0 1px oklch(0% 0 0 / 60%),
    0 12px 40px oklch(0% 0 0 / 55%),
    0 0 20px var(--green-glow);
  transition:
    border-top-color 0.4s ease,
    box-shadow 0.4s ease;
}

/* Snapped 吸附闪光 */
.widget.snapped {
  animation: snap-flash 0.4s var(--ease-out-quint);
}

@keyframes snap-flash {
  0% {
    box-shadow:
      0 0 0 1px oklch(0% 0 0 / 60%),
      0 0 0 oklch(70% 0.17 63 / 0%);
  }
  40% {
    box-shadow:
      0 0 0 1px oklch(0% 0 0 / 60%),
      0 0 36px oklch(70% 0.17 63 / 55%);
  }
  100% {
    box-shadow:
      0 0 0 1px oklch(0% 0 0 / 60%),
      0 12px 40px oklch(0% 0 0 / 55%),
      0 0 20px oklch(70% 0.17 63 / 25%);
  }
}

/* ── 扫描线覆盖层 ────────────────────────── */
.scan-overlay {
  position: absolute;
  inset: 0;
  pointer-events: none;
  z-index: 10;
  overflow: hidden;
}

.scan-line {
  position: absolute;
  left: 0;
  right: 0;
  top: 0;
  height: 1px;
  background: linear-gradient(
    90deg,
    transparent 0%,
    var(--accent) 30%,
    oklch(85% 0.2 63) 50%,
    var(--accent) 70%,
    transparent 100%
  );
  opacity: 0;
  animation: scan-sweep 1.6s linear infinite;
}

@keyframes scan-sweep {
  0% {
    transform: translateY(0px);
    opacity: 0;
  }
  5% {
    opacity: 0.7;
  }
  90% {
    opacity: 0.5;
  }
  100% {
    transform: translateY(200px);
    opacity: 0;
  }
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
  position: relative;
  z-index: 2;
}
.bar:active {
  cursor: grabbing;
}

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
  transition:
    background 0.25s ease,
    box-shadow 0.25s ease;
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
  transition:
    color 0.15s ease,
    border-color 0.15s ease,
    background 0.15s ease;
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
  position: relative;
  z-index: 1;
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
  transition: color 0.2s ease;
}

/* 数值变化闪光 */
.val.flash {
  animation: val-flash 0.5s var(--ease-out-quint);
}

@keyframes val-flash {
  0% {
    color: var(--accent);
    text-shadow: none;
  }
  25% {
    color: oklch(92% 0.2 63);
    text-shadow:
      0 0 8px oklch(80% 0.22 63 / 80%),
      0 0 20px oklch(70% 0.17 63 / 40%);
    transform: scale(1.04);
  }
  100% {
    color: var(--accent);
    text-shadow: none;
    transform: scale(1);
  }
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

/* 状态切换闪光 (叠加在 state-val 动画上) */
.state-val.flash {
  animation: state-flash 0.6s var(--ease-out-quint);
}
.state-val.scanning.flash {
  animation:
    state-flash 0.6s var(--ease-out-quint),
    pulse 1s ease-in-out infinite 0.6s;
}

@keyframes state-flash {
  0% {
    opacity: 0;
    transform: translateY(-4px) scale(0.9);
    filter: blur(2px);
  }
  60% {
    opacity: 1;
    transform: translateY(1px) scale(1.02);
    filter: blur(0);
  }
  100% {
    opacity: 1;
    transform: translateY(0) scale(1);
    filter: blur(0);
  }
}

@keyframes pulse {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.4;
  }
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

.vu-lbl,
.vu-db {
  font-size: 9px;
  font-weight: 600;
  letter-spacing: 1px;
  color: var(--muted);
  flex-shrink: 0;
  width: 22px;
}
.vu-db {
  text-align: right;
}

.vu-meter {
  flex: 1;
  display: flex;
  gap: 2px;
  align-items: center;
}

/* 亮起时快速响应，熄灭时缓慢衰减 */
.seg {
  flex: 1;
  height: 8px;
  border-radius: 1px;
  background: var(--seg-off);
  /* 熄灭时慢衰减 */
  transition:
    background 0.28s ease,
    box-shadow 0.28s ease;
}
.seg.on {
  background: var(--accent);
  box-shadow: 0 0 4px var(--accent);
  /* 亮起时快速响应 */
  transition:
    background 0.05s ease,
    box-shadow 0.05s ease;
}
.seg.on.warn {
  background: var(--seg-warn);
  box-shadow: 0 0 5px oklch(72% 0.19 45 / 70%);
}
.seg.on.danger {
  background: var(--seg-danger);
  box-shadow: 0 0 6px oklch(63% 0.24 22 / 80%);
  animation: seg-danger-pulse 0.4s ease-in-out infinite alternate;
}

@keyframes seg-danger-pulse {
  from {
    box-shadow: 0 0 4px oklch(63% 0.24 22 / 60%);
  }
  to {
    box-shadow:
      0 0 8px oklch(63% 0.24 22 / 90%),
      0 0 14px oklch(63% 0.24 22 / 40%);
  }
}

/* ── 操作按钮 ──────────────────────────────── */
.actions {
  display: flex;
  gap: 8px;
}

.btn-fill,
.btn-ghost {
  flex: 1;
  height: 28px;
  border-radius: 2px;
  font-family: inherit;
  font-size: 11px;
  font-weight: 700;
  letter-spacing: 1.5px;
  cursor: pointer;
  transition: all 0.15s ease;
  position: relative;
  overflow: hidden;
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
  transform: scale(0.97);
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
  transform: scale(0.97);
}

/* 点击涟漪 */
:deep(.ripple) {
  position: absolute;
  transform: translate(-50%, -50%) scale(0);
  width: 120px;
  height: 120px;
  border-radius: 50%;
  background: oklch(100% 0 0 / 18%);
  pointer-events: none;
  animation: ripple-out 0.65s var(--ease-out-quint) forwards;
}

@keyframes ripple-out {
  to {
    transform: translate(-50%, -50%) scale(2.5);
    opacity: 0;
  }
}

/* ── 无障碍: 减少动画 ──────────────────────── */
@media (prefers-reduced-motion: reduce) {
  .widget {
    animation: none;
  }
  .scan-line {
    animation: none;
    display: none;
  }
  .val.flash,
  .state-val.flash {
    animation: none;
  }
  .seg,
  .seg.on {
    transition-duration: 0.01ms;
  }
  .seg.on.danger {
    animation: none;
  }
  .state-val.scanning {
    animation: none;
    opacity: 0.7;
  }
  :deep(.ripple) {
    display: none;
  }
}
</style>
