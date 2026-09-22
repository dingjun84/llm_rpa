# llm_rpa Mac 交接文档（2026-09-21）

给**下一个 agent / 接手的人**：先读本文，再动代码。不要在 Linux box 上编/测 Mac OCR 或桌面端。

---

## 1. 环境与仓库

| 项 | 值 |
|---|---|
| 机器 | 用户本机 Mac，`machineId`：`38491682-5f77-4611-ac01-d1ee5fb6d28f` |
| 路径 | `/Users/admin/Desktop/workspace/llm_rpa` |
| 分支 | `feat/macos-platform`（相对 origin **ahead 1**，另有大量未提交改动） |
| GitHub | 用户 `dingjun84`；助手电脑 SSH 公钥已加，可用 SSH |
| Cloud Agent | 需要 Pro，当前不可用 → **本地改代码** |
| 启动 | `./run.sh`；强制重建 `./run.sh --rebuild` |
| PATH | `$HOME/.cargo/bin:$HOME/.local/node/bin:/usr/bin:$PATH`（Node 在 `~/.local/node`，勿依赖慢 Homebrew） |

### OCR 路径（配置页「本地 OCR 程序路径」）

- **Mac**：`/Users/admin/Desktop/workspace/llm_rpa/target/debug/macosocr`（无 `.exe`）
- **Windows**：`<仓库>/target/debug/winocr.exe`
- 源码：`tools/macosocr/`；`run.sh` 会按需用 `swiftc` 编译

### 图标库目录

常见为 `/Users/admin/Desktop/workspace/llm_rpa/data`（下有 `聊天`、`通讯录` 等子目录）。**不是**固定的 `data/icons/`；以配置里「图标库目录」与界面列表为准。

---

## 2. 产品约定（接手后不要推翻）

1. **搜索式**（`SearchContact`）：**一律**先点「用于联系人导航」图标（通讯录/联系人），再搜、点下拉、资料页「发消息」、填输入框，然后**人工确认 → 发送 → 核验送达**。
   ⚠️ 2026-09-22 起它**不再**无条件停在 `Prepared`：只有勾了「只填不发」才停。
   演练模式仍然不会真的发出去——那由替身端口保证，不是靠流程提前停（见 `docs/todo.md` T16 末尾）。
2. **列表扫描式**（`ScrollListContact`）：**一律**先点「用于对话历史导航」图标（聊天/微信），再扫会话列表。
3. **只做导航**（`NavigateOnly`）：任务页另选图标；与上面两套勾选无关。
4. 两套勾选独立：
   - `nav_icon_templates` ← UI「用于联系人导航」
   - `chat_history_nav_templates` ← UI「用于对话历史导航」
5. 未勾联系人导航时，装配期回退库内名 `通讯录` / `联系人`；对话历史回退 `聊天` / `微信`。建议仍手动勾上并保存。
6. 搜索下拉默认分组标题：`联系人 / 最常使用`（可多项，分隔符 `/`、`、`、空白）。只在分组标题与**下一组**（群聊/聊天记录/公众号/小程序等）之间找人，避免点「聊天记录」同名。
7. Mac 逐字输入：用 `CGEventKeyboardSetUnicodeString`，**不要**再改回每字剪贴板+Cmd+V（会联想覆盖）。
8. 匹配：当前图标库是**单尺度**像素匹配；1.0 外接屏与 2.0 Retina 需各截模板。界面标定按显示器 scale 分档，**不能替代**多套图标模板。
9. `scale_factor` = **窗口所在屏**缩放（经 `measure_target_window` / `metrics_for_rect`），不是主屏。
10. 指认窗口在**界面标定**页；Mac 在包含光标的窗口里取**面积最小**者（不必获焦）。

---

## 3. 已落地（本轮及近期，未全部 commit）

### 桌面 / 运行时

- 多份标定按 scale 存/选；任务页去掉重复区域参数，引用标定页。
- 新任务自动切最新过程日志；展示操作者；`data/task-*.log` 恢复历史。
- 图标库双勾选 + 删除时从两套列表摘掉。
- `build_runner`：NavigateOnly / SearchContact / ScrollListContact 分别装导航模板。
- 搜索下拉多分组 + 下一节截断（`runner/search.rs`）。
- Mac Retina：截图/匹配/点击同一坐标系；鼠标 move 再点；定位并点击两段确认。
- 资料页已 OCR 到「发消息」则跳过滚动。

### 测试（本机 Mac 近一次）

- `cargo test -p automation-core --lib` → 42 passed  
- `cargo test -p platform-mock --test mvp_flow` → 73 passed  
- `cargo test -p desktop --lib` → 95 passed  
- `./run.sh --rebuild` 已成功；OCR：`target/debug/macosocr`

### 关键文件（改动集中处）

| 区域 | 路径 |
|---|---|
| 搜索下拉分组 | `crates/automation-core/src/runner/search.rs` |
| 工作流 / must_navigate | `crates/automation-core/src/runner/mod.rs` |
| 装配导航 | `apps/desktop/src-tauri/src/runtime.rs`（含 `resolve_nav_icon_names`、`chat_history_nav_templates`） |
| 装配测试 | `apps/desktop/src-tauri/src/runtime/tests.rs` |
| 图标 UI | `IconList.tsx` / `IconLibraryPanel.tsx` |
| 分组标题配置文案 | `TypingTextSection.tsx`、`types.ts` |
| Mac 桌面/输入 | `crates/platform-macos/` |
| 视觉匹配 | `crates/vision/src/template.rs` |
| 启动 | `run.sh`、`docs/macos.md` |

未跟踪：`runtime/calibrations.rs`、`CalibrationProfiles.tsx`、`crates/vision/src/layout.rs`、`install.sh` / `install2.sh`。

---

## 4. 操作者怎么配（验收前检查）

1. OCR 填 Mac 路径并**保存配置**。  
2. 图标库：给「通讯录」勾 **用于联系人导航**；给「聊天」勾 **用于对话历史导航**；保存。Retina/外接各有模板。  
3. 搜索分组标题：`联系人 / 最常使用`（或按客户端改）。  
4. 界面标定：当前屏重新记窗口与区域；搜索式要 `main_search` / `search_dropdown` / `contact_profile`。  
5. 复测建议：搜索式找人（含只出现在「最常使用」）；列表扫描式「只做导航→通讯录」与完整列表扫。

---

## 5. 已知坑 / 不要重蹈

- **Linux box ≠ Mac**：曾在 box 改完、Mac 没同步；用户要求 Mac 工作一律在本机测。  
- 搜索式 mock 测试点击序列含**导航那一下**（共 5 次点击），改 `must_navigate` 时同步改 `mvp_flow.rs`。  
- `nav_target_label`：列表扫描装载后是**图标名**（如「聊天」），不是固定文案「聊天历史」。  
- Windows 绝对路径用例（`D:/...`）在 Mac 上要用 `cfg(windows)` / Unix 绝对路径分支（已在 `icon_library/tests.rs` 处理）。  
- 金字塔多尺度匹配代码可能仍在树里但当前产品路径偏向单尺度+分缩放模板；改匹配策略前先确认产品意图。  
- 未 commit 的改动很多；接手若要推远程，先 `git status` / diff，按用户意图拆 commit，**不要擅自 force push**。

---

## 6. 建议下一个 agent 优先做的事（按价值）

用户意图：额度紧，把难的、重要的留给当前能干活的 agent；常规收尾可换人。下面按优先级：

### P0 — 实机验收与稳定性（难，用户价值高）

1. 带着真实微信/企微窗口跑通：**搜索式**（联系人组 + 仅最常使用）与 **列表扫描式**（先点聊天）。  
2. 失败时看任务页「停止原因」+「过程日志」末几行，对着 `task-*.log` 修坐标/OCR/分组，而不是盲改默认值。  
3. Retina vs 外接：缺模板时的报错是否够清楚；是否要按 scale 分目录约定写进 UI 提示。

### P1 — 工程债（中等）

1. 把 `feat/macos-platform` 上未提交的 Mac 相关改动整理成可审查的 commits（或按用户要求先不推）。  
2. 对齐 `docs/todo.md` / `docs/macos.md` 与现状（双勾选、双分组、OCR 路径）。  
3. 清理死代码警告（如 vision 里未用的 pyramid 函数、`Emitter` unused）。

### P2 — 功能扩展（需用户拍板）

1. 发送动作仍停在 Prepared——加发送前再与用户确认。  
2. 姓名匹配仍有「放宽包含」开关，生产发送前必须关掉（见配置注释与 architecture）。  
3. 企业微信 vs 微信文案/窗口类名差异是否要分 profile。

---

## 7. 给下一个 agent 的最短指令

```
仓库：/Users/admin/Desktop/workspace/llm_rpa ，分支 feat/macos-platform。
只在用户 Mac（machineId 38491682-5f77-4611-ac01-d1ee5fb6d28f）上改与测。
读 docs/handoff-macos-2026-09-21.md。
启动：PATH 含 ~/.cargo/bin 与 ~/.local/node/bin 后 ./run.sh --rebuild。
OCR：target/debug/macosocr。
先 git status，大量本地改动未提交；不要在 Linux box 上测 Mac。
```

---

## 8. 文档索引

| 文档 | 用途 |
|---|---|
| `docs/macos.md` | Mac 构建与 OCR |
| `docs/todo.md` | 长期取舍与待办（权威列表） |
| `docs/code-map.md` | 模块地图 |
| `docs/windows-mvp-interface.md` | 标定区域说明（Win 为主，区域 key 通用） |
| 本文 | **Mac 现状交接** |

