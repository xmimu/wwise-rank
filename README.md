# Wwise Rank

> 为 Wwise 音频工程师设计的实时活动监控与积分悬浮窗

Wwise Rank 是一款基于 Tauri 的轻量级桌面悬浮窗应用，通过 WAAPI（Wwise Authoring API）实时监听你在 Wwise 中的操作行为，并以游戏化积分的方式记录工作量，帮助你直观感知当前的工作节奏与效率。

---

## 功能亮点

- **实时连接** — 自动扫描并连接本机运行的 Wwise 实例（支持端口 8080–8083），断连后自动重试
- **积分系统** — 22 种 WAAPI 操作事件各有不同分值，按操作复杂度加权（1–5 分）
- **双模式计分** — 区分「总排名分（Rank）」（跨会话持久化）与「本次会话分（Session）」
- **VU 表头** — 24 段可视化活跃度指示器，直观反映近期操作频率
- **悬浮置顶** — 320×200 无边框透明窗口，可拖拽、可吸附到屏幕边缘、支持固定/取消置顶
- **自定义积分** — 通过 `score_config.json` 可覆盖任意事件的默认分值

---

## 快速开始

### 环境要求

| 工具 | 版本要求 |
|------|----------|
| Rust | 1.70+ |
| Node.js | 18+ |
| pnpm | 8+ |
| Wwise | 已启用 WAAPI（本地 WebSocket） |

### 开发运行

```bash
pnpm install
pnpm tauri dev
```

### 构建发布包

```bash
pnpm tauri build
```

构建产物位于 `src-tauri/target/release/bundle/`。

---

## 使用说明

详见 [用户手册](docs/用户手册.md)，涵盖：

- 界面说明与操作方式
- 积分规则与计分表（[操作计分表](docs/操作计分表.md)）
- 自定义积分配置
- 数据持久化说明
- 高级环境变量配置

---

## 技术栈

| 层 | 技术 |
|----|------|
| 前端 | Vue 3 + TypeScript + Vite |
| 后端 | Rust + Tauri 2 |
| 异步运行时 | Tokio |
| WAAPI 客户端 | waapi-rs |
| 序列化 | serde / serde_json |

---

## 数据文件

应用运行时会在系统应用数据目录下生成以下文件：

| 文件 | 说明 |
|------|------|
| `score.json` | 总分与各事件触发计数（持久化） |
| `score_config.json` | 用户自定义积分配置（首次启动自动生成） |

---

## 开发 IDE 推荐

VS Code + 以下插件：

- [Vue - Official](https://marketplace.visualstudio.com/items?itemName=Vue.volar)
- [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode)
- [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer)
