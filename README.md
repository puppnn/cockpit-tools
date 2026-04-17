# cockpit-tools (puppnn fork)

[English](README.en.md) · 简体中文

这是基于 [jlcodes99/cockpit-tools](https://github.com/jlcodes99/cockpit-tools) 的定制 fork。

这个 fork 当前主要补强的是 Codex 模块，重点是本地会话读取、查看、标题编辑和收藏备份。完整定制代码在 [`my-main`](https://github.com/puppnn/cockpit-tools/tree/my-main) 分支。

## 本 fork 修改点

### 1. Codex 会话完整读取

- 聚合读取 `sessions/**/*.jsonl`
- 合并 `session_index.jsonl`
- 合并 `state_5.sqlite`
- 尽量把 `~/.codex` 下的本地会话完整识别出来

### 2. Codex 三栏会话查看

- 左侧按工作目录分组显示会话
- 中间仅保留用户提问和助手回复
- 右侧用于标题编辑和会话详情查看
- 三栏支持左右拖拽
- 左、中、右三栏分别滚动，互不干扰

### 3. 会话内容阅读优化

- 默认隐藏用户消息开头的 `<environment_context>...</environment_context>`
- 长消息默认折叠
- 中间区域尽量减少元信息，优先快速看懂这个会话在做什么

### 4. 会话标题编辑

- 保存标题时直接修改原始 rollout 会话文件中的 `session_meta`
- 同步更新 `session_index.jsonl`
- 同步更新 `state_5.sqlite`
- 不在保存标题时自动备份

### 5. 收藏与备份

- 新增“收藏 / 取消收藏”按钮
- 收藏后会把对应会话备份到 `~/.codex/cockpit-tools-session-favorites`
- 取消收藏时只删除备份，不改动原始会话文件

### 6. Windows 本地调试体验

- 新增 `run-tauri-dev.cmd`
- 新增 `run-tauri-dev-hidden.vbs`
- Windows 下可双击启动本地调试，并尽量避免弹出黑框

## 分支说明

- [`my-main`](https://github.com/puppnn/cockpit-tools/tree/my-main): 你的定制开发分支，包含完整功能实现
- `main`: 当前 fork 首页展示分支，也同步这份 fork README
- `upstream`: 原作者仓库 `jlcodes99/cockpit-tools`

## 本地运行

```bash
npm install
npm run tauri dev
```

Windows 下也可以直接双击：

```bash
run-tauri-dev-hidden.vbs
```

日志会写到仓库根目录的 `dev-tauri.log`。

## 上游项目

- 原作者仓库: https://github.com/jlcodes99/cockpit-tools
- 我的 fork: https://github.com/puppnn/cockpit-tools

