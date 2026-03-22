<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick } from "vue";
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

interface TopicSetting {
  uri: string;
  score: number;
  enabled: boolean;
}

interface UserSettings {
  port_start: number;
  port_end: number;
  topics: TopicSetting[];
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

// Language
type Lang = "zh" | "en";
const lang = ref<Lang>("zh");

const i18n = {
  zh: {
    pinTitle: "置顶",
    closeTitle: "关闭",
    portRangePlaceholder: "8080 或 8080-8090",
    portRangeError: "格式无效，请输入如 8080 或 8080-8090",
    portRangeLabel: "PORT RANGE",
    uriSubLabel: "URI SUBSCRIPTIONS",
    resetScore: "RESET TOTAL SCORE",
    resetConfirm: "再次点击确认重置",
    restoreDefaultsTitle: "恢复默认设置",
    langLabel: "LANGUAGE",
  },
  en: {
    pinTitle: "Pin",
    closeTitle: "Close",
    portRangePlaceholder: "8080 or 8080-8090",
    portRangeError: "Invalid format, use e.g. 8080 or 8080-8090",
    portRangeLabel: "PORT RANGE",
    uriSubLabel: "URI SUBSCRIPTIONS",
    resetScore: "RESET TOTAL SCORE",
    resetConfirm: "Click again to confirm reset",
    restoreDefaultsTitle: "Restore defaults",
    langLabel: "LANGUAGE",
  },
};

const t = computed(() => i18n[lang.value]);

function setLang(l: Lang) {
  lang.value = l;
  localStorage.setItem("wwise-rank-lang", l);
}

// Settings panel state
const showSettings = ref(false);
const settingsDraft = ref<UserSettings | null>(null);
const portRangeInput = ref("");
const resetConfirm = ref(false);
let resetConfirmTimer: ReturnType<typeof setTimeout> | null = null;

const MAIN_HEIGHT = 200;
const SETTINGS_HEIGHT = 420;

// Track logical window height ourselves so snapToEdge never reads a stale outerSize().
const windowLogicalH = ref(MAIN_HEIGHT);

const SNAP_PX = 80;
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
    const eased = 1 - (1 - t) ** 3;
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
  const [pos, monitor] = await Promise.all([
    appWindow.outerPosition(),
    currentMonitor(),
  ]);
  if (!monitor) return;

  const scale = monitor.scaleFactor;
  const mx = monitor.position.x;
  const my = monitor.position.y;
  const mw = monitor.size.width;
  const mh = monitor.size.height;
  // Use our tracked logical size to avoid stale outerSize() readings after resize.
  const ww = Math.round(320 * scale);
  const wh = Math.round(windowLogicalH.value * scale);

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

// ── Settings panel ────────────────────────────────────────────────────────────

const TOPIC_DESCRIPTIONS_ZH: Record<string, string> = {
  "ak.wwise.core.audio.imported":                       "音频文件导入时",
  "ak.wwise.core.object.attenuationCurveChanged":       "衰减曲线修改时",
  "ak.wwise.core.object.attenuationCurveLinkChanged":   "衰减曲线链接变更时",
  "ak.wwise.core.object.childAdded":                    "子对象添加时",
  "ak.wwise.core.object.childRemoved":                  "子对象移除时",
  "ak.wwise.core.object.created":                       "对象创建时",
  "ak.wwise.core.object.curveChanged":                  "曲线修改时",
  "ak.wwise.core.object.nameChanged":                   "对象重命名时",
  "ak.wwise.core.object.notesChanged":                  "备注修改时",
  "ak.wwise.core.object.postDeleted":                   "对象删除后",
  "ak.wwise.core.object.propertyChanged":               "对象属性修改时",
  "ak.wwise.core.object.referenceChanged":              "对象引用变更时",
  "ak.wwise.core.object.structureChanged":              "对象结构变更时",
  "ak.wwise.core.project.loaded":                       "项目加载时",
  "ak.wwise.core.project.postClosed":                   "项目关闭后",
  "ak.wwise.core.project.saved":                        "项目保存时",
  "ak.wwise.core.soundbank.generated":                  "Sound Bank 生成时",
  "ak.wwise.core.soundbank.generationDone":             "Sound Bank 生成完成时",
  "ak.wwise.core.switchContainer.assignmentAdded":      "Switch 容器分配添加时",
  "ak.wwise.core.switchContainer.assignmentRemoved":    "Switch 容器分配移除时",
  "ak.wwise.core.transport.stateChanged":               "传输状态变更时",
  "ak.wwise.ui.selectionChanged":                       "选中对象变更时",
};

const TOPIC_DESCRIPTIONS_EN: Record<string, string> = {
  "ak.wwise.core.audio.imported":                       "Audio file imported",
  "ak.wwise.core.object.attenuationCurveChanged":       "Attenuation curve changed",
  "ak.wwise.core.object.attenuationCurveLinkChanged":   "Attenuation curve link changed",
  "ak.wwise.core.object.childAdded":                    "Child object added",
  "ak.wwise.core.object.childRemoved":                  "Child object removed",
  "ak.wwise.core.object.created":                       "Object created",
  "ak.wwise.core.object.curveChanged":                  "Curve changed",
  "ak.wwise.core.object.nameChanged":                   "Object renamed",
  "ak.wwise.core.object.notesChanged":                  "Notes changed",
  "ak.wwise.core.object.postDeleted":                   "Object deleted",
  "ak.wwise.core.object.propertyChanged":               "Property changed",
  "ak.wwise.core.object.referenceChanged":              "Reference changed",
  "ak.wwise.core.object.structureChanged":              "Structure changed",
  "ak.wwise.core.project.loaded":                       "Project loaded",
  "ak.wwise.core.project.postClosed":                   "Project closed",
  "ak.wwise.core.project.saved":                        "Project saved",
  "ak.wwise.core.soundbank.generated":                  "Sound bank generated",
  "ak.wwise.core.soundbank.generationDone":             "Sound bank generation done",
  "ak.wwise.core.switchContainer.assignmentAdded":      "Switch container assignment added",
  "ak.wwise.core.switchContainer.assignmentRemoved":    "Switch container assignment removed",
  "ak.wwise.core.transport.stateChanged":               "Transport state changed",
  "ak.wwise.ui.selectionChanged":                       "Selection changed",
};

function topicLabel(uri: string): string {
  const parts = uri.split(".");
  return parts.slice(-2).join(".");
}

function topicDescription(uri: string): string {
  const map = lang.value === "en" ? TOPIC_DESCRIPTIONS_EN : TOPIC_DESCRIPTIONS_ZH;
  return map[uri] ?? uri;
}

function parsePortRange(s: string): { start: number; end: number } | null {
  const single = s.match(/^(\d{1,5})$/);
  if (single) {
    const p = parseInt(single[1]);
    if (p >= 1 && p <= 65535) return { start: p, end: p };
  }
  const range = s.match(/^(\d{1,5})-(\d{1,5})$/);
  if (range) {
    const a = parseInt(range[1]);
    const b = parseInt(range[2]);
    if (a >= 1 && a <= 65535 && b >= a && b <= 65535) return { start: a, end: b };
  }
  return null;
}

const portRangeError = ref("");

async function openSettings() {
  const s = await invoke<UserSettings>("get_settings");
  settingsDraft.value = JSON.parse(JSON.stringify(s)) as UserSettings;
  portRangeInput.value =
    s.port_start === s.port_end ? `${s.port_start}` : `${s.port_start}-${s.port_end}`;
  portRangeError.value = "";
  showSettings.value = true;
  windowLogicalH.value = SETTINGS_HEIGHT;
  await invoke("set_window_height", { height: SETTINGS_HEIGHT });
}

async function closeSettings() {
  showSettings.value = false;
  settingsDraft.value = null;
  if (resetConfirmTimer) clearTimeout(resetConfirmTimer);
  resetConfirm.value = false;
  await nextTick(); // wait for DOM to shrink before resizing the window
  windowLogicalH.value = MAIN_HEIGHT;
  await invoke("set_window_height", { height: MAIN_HEIGHT });
}

async function applySettings() {
  if (!settingsDraft.value) return;
  const parsed = parsePortRange(portRangeInput.value.trim());
  if (!parsed) {
    portRangeError.value = t.value.portRangeError;
    return;
  }
  const payload: UserSettings = {
    port_start: parsed.start,
    port_end: parsed.end,
    topics: settingsDraft.value.topics,
  };
  await invoke("save_settings", { newSettings: payload });
  await closeSettings();
}

async function resetToDefaults() {
  const defaults = await invoke<UserSettings>("get_default_settings");
  settingsDraft.value = defaults;
  portRangeInput.value =
    defaults.port_start === defaults.port_end
      ? `${defaults.port_start}`
      : `${defaults.port_start}-${defaults.port_end}`;
  portRangeError.value = "";
}

async function resetScore() {
  if (!resetConfirm.value) {
    // First click: ask for confirmation
    resetConfirm.value = true;
    if (resetConfirmTimer) clearTimeout(resetConfirmTimer);
    resetConfirmTimer = setTimeout(() => {
      resetConfirm.value = false;
    }, 3000);
    return;
  }
  // Second click: execute
  if (resetConfirmTimer) clearTimeout(resetConfirmTimer);
  resetConfirm.value = false;
  await invoke("reset_total_score");
}

onMounted(async () => {
  const savedLang = localStorage.getItem("wwise-rank-lang") as Lang | null;
  if (savedLang === "zh" || savedLang === "en") lang.value = savedLang;

  unlistenMoved = await appWindow.onMoved(() => {
    if (snapTimer) clearTimeout(snapTimer);
    snapTimer = setTimeout(snapToEdge, 120);
  });

  try {
    const initial = await invoke<ScorePayload>("get_score");
    applyPayload(initial);
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
  if (resetConfirmTimer) clearTimeout(resetConfirmTimer);
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
      'settings-open': showSettings,
    }"
    @contextmenu.prevent
  >
    <!-- 扫描线覆盖层 (仅 Scanning 状态) -->
    <div v-if="connState === 'Scanning' && !showSettings" class="scan-overlay" aria-hidden="true">
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
          :title="t.pinTitle"
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
          :title="t.closeTitle"
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

    <!-- 主视图 -->
    <main v-if="!showSettings" class="body">
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
        <button class="btn-fill" @mousedown="createRipple">ANALYZE</button>
        <button class="btn-ghost" @mousedown="createRipple" @click="openSettings">SETTINGS</button>
      </div>
    </main>

    <!-- 设置面板 -->
    <section v-else-if="settingsDraft" class="settings-body">
      <!-- 端口范围 -->
      <div class="s-section">
        <div class="s-label">{{ t.portRangeLabel }}</div>
        <div class="s-port-row">
          <input
            v-model="portRangeInput"
            class="s-input"
            :placeholder="t.portRangePlaceholder"
            spellcheck="false"
            @input="portRangeError = ''"
          />
        </div>
        <div v-if="portRangeError" class="s-error">{{ portRangeError }}</div>
      </div>

      <!-- URI 订阅列表 -->
      <div class="s-section s-section-grow">
        <div class="s-label">{{ t.uriSubLabel }}</div>
        <div class="s-topic-list">
          <label
            v-for="(topic, idx) in settingsDraft.topics"
            :key="topic.uri"
            class="s-topic-row"
            :class="{ disabled: !topic.enabled }"
          >
            <input
              type="checkbox"
              class="s-checkbox"
              :checked="topic.enabled"
              @change="settingsDraft!.topics[idx].enabled = ($event.target as HTMLInputElement).checked"
            />
            <span class="s-topic-name" :title="topicDescription(topic.uri)">{{ topicLabel(topic.uri) }}</span>
            <input
              type="number"
              class="s-score-input"
              :value="topic.score"
              min="0"
              max="9999"
              :disabled="!topic.enabled"
              @change="settingsDraft!.topics[idx].score = Math.max(0, parseInt(($event.target as HTMLInputElement).value) || 0)"
            />
          </label>
        </div>
      </div>

      <!-- 重置总分 -->
      <div class="s-section">
        <button
          class="btn-reset"
          :class="{ 'btn-reset-confirm': resetConfirm }"
          @click="resetScore"
        >
          {{ resetConfirm ? t.resetConfirm : t.resetScore }}
        </button>
      </div>

      <!-- 语言切换 -->
      <div class="s-section s-lang-section">
        <div class="s-label">{{ t.langLabel }}</div>
        <div class="s-lang-toggle">
          <button :class="['s-lang-btn', { active: lang === 'zh' }]" @click="setLang('zh')">中文</button>
          <button :class="['s-lang-btn', { active: lang === 'en' }]" @click="setLang('en')">EN</button>
        </div>
      </div>

      <!-- 保存 / 取消 / 恢复默认 -->
      <div class="s-actions">
        <button class="btn-fill s-btn" @mousedown="createRipple" @click="applySettings">SAVE</button>
        <button class="btn-ghost s-btn" @mousedown="createRipple" @click="closeSettings">CANCEL</button>
        <button class="btn-ghost s-btn s-btn-defaults" @mousedown="createRipple" @click="resetToDefaults" :title="t.restoreDefaultsTitle">↺</button>
      </div>
    </section>
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

.widget.settings-open {
  height: 420px;
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

/* Connected 状态 */
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

.seg {
  flex: 1;
  height: 8px;
  border-radius: 1px;
  background: var(--seg-off);
  transition:
    background 0.28s ease,
    box-shadow 0.28s ease;
}
.seg.on {
  background: var(--accent);
  box-shadow: 0 0 4px var(--accent);
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

/* ── 设置面板 ──────────────────────────────── */
.settings-body {
  flex: 1;
  display: flex;
  flex-direction: column;
  padding: 10px 14px 10px;
  gap: 8px;
  overflow: hidden;
}

.s-section {
  display: flex;
  flex-direction: column;
  gap: 5px;
}

.s-section-grow {
  flex: 1;
  min-height: 0;
}

.s-label {
  font-size: 9px;
  font-weight: 700;
  letter-spacing: 1.8px;
  color: var(--muted);
  text-transform: uppercase;
}

.s-port-row {
  display: flex;
  gap: 6px;
}

.s-input {
  flex: 1;
  height: 26px;
  background: oklch(100% 0 0 / 4%);
  border: 1px solid var(--border);
  border-radius: 2px;
  color: var(--text);
  font-family: inherit;
  font-size: 12px;
  font-weight: 600;
  letter-spacing: 0.5px;
  padding: 0 8px;
  outline: none;
  transition: border-color 0.15s ease;
}
.s-input:focus {
  border-color: var(--a-dim);
}
.s-input::placeholder {
  color: oklch(35% 0.01 255);
}

.s-error {
  font-size: 9px;
  color: var(--seg-danger);
  letter-spacing: 0.3px;
}

/* 滚动列表 */
.s-topic-list {
  flex: 1;
  overflow-y: auto;
  border: 1px solid var(--border);
  border-radius: 2px;
  background: oklch(100% 0 0 / 2%);
  scrollbar-width: thin;
  scrollbar-color: var(--border) transparent;
}

.s-topic-row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 0 8px;
  height: 30px;
  border-bottom: 1px solid oklch(22% 0.015 255 / 50%);
  cursor: pointer;
  transition: background 0.1s ease;
}
.s-topic-row:last-child {
  border-bottom: none;
}
.s-topic-row:hover {
  background: oklch(100% 0 0 / 3%);
}
.s-topic-row.disabled .s-topic-name {
  color: oklch(30% 0.01 255);
}

.s-checkbox {
  flex-shrink: 0;
  width: 12px;
  height: 12px;
  cursor: pointer;
  accent-color: var(--accent);
}

.s-topic-name {
  flex: 1;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.3px;
  color: var(--text);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.s-score-input {
  width: 44px;
  height: 22px;
  background: oklch(100% 0 0 / 5%);
  border: 1px solid var(--border);
  border-radius: 2px;
  color: var(--accent);
  font-family: inherit;
  font-size: 11px;
  font-weight: 700;
  font-feature-settings: "tnum";
  text-align: center;
  outline: none;
  padding: 0 4px;
  transition: border-color 0.15s ease;
  /* hide spinners */
  -moz-appearance: textfield;
  appearance: textfield;
}
.s-score-input::-webkit-inner-spin-button,
.s-score-input::-webkit-outer-spin-button {
  -webkit-appearance: none;
}
.s-score-input:focus {
  border-color: var(--a-dim);
}
.s-score-input:disabled {
  color: oklch(30% 0.01 255);
  border-color: oklch(18% 0.01 255);
}

/* 重置按钮 */
.btn-reset {
  width: 100%;
  height: 26px;
  border-radius: 2px;
  font-family: inherit;
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 1.2px;
  cursor: pointer;
  background: transparent;
  border: 1px solid oklch(40% 0.18 22);
  color: oklch(60% 0.18 22);
  transition: all 0.15s ease;
  position: relative;
  overflow: hidden;
}
.btn-reset:hover {
  background: oklch(60% 0.18 22 / 10%);
  border-color: oklch(55% 0.2 22);
  color: oklch(70% 0.2 22);
}
.btn-reset.btn-reset-confirm {
  border-color: oklch(63% 0.24 22);
  color: oklch(63% 0.24 22);
  background: oklch(63% 0.24 22 / 12%);
  animation: reset-pulse 0.6s ease-in-out infinite alternate;
}

@keyframes reset-pulse {
  from { box-shadow: none; }
  to { box-shadow: 0 0 8px oklch(63% 0.24 22 / 40%); }
}

.s-actions {
  display: flex;
  gap: 8px;
}

.s-btn-defaults {
  flex: none;
  width: 32px;
  font-size: 14px;
  letter-spacing: 0;
}

.s-btn {
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

/* ── 语言切换 ──────────────────────────────── */
.s-lang-section {
  flex-direction: row;
  align-items: center;
  justify-content: space-between;
}

.s-lang-toggle {
  display: flex;
  gap: 4px;
}

.s-lang-btn {
  height: 20px;
  padding: 0 8px;
  border-radius: 2px;
  font-family: inherit;
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.8px;
  cursor: pointer;
  background: transparent;
  border: 1px solid var(--border);
  color: var(--muted);
  transition: all 0.15s ease;
}
.s-lang-btn:hover {
  border-color: oklch(35% 0.02 255);
  color: var(--text);
}
.s-lang-btn.active {
  background: var(--a-glow);
  border-color: var(--a-dim);
  color: var(--accent);
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
  .btn-reset.btn-reset-confirm {
    animation: none;
  }
}
</style>
