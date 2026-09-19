# 代码地图：改 X 该打开哪个文件

> **这份文档只做一件事**：把「要改的东西」映射到「文件 + 符号」。
> 它**不讲设计理由**（理由在 `architecture.md`），**不记进度和取舍**（在 `todo.md`），
> **不写实测细节和踩坑**（在 `.workbuddy-ai/memory/REFERENCE.md`）。
>
> **给新会话的用法**：先读这里定位到 1~3 个文件，再按需读那几个文件。
> 不要一上来就 Glob/Grep 全库——仓库约 1.8 万行源码、48 个源文件，
> 其中 5 个文件占了四成代码量，而 90% 的改动只落在那 5 个里。
>
> **锚点是符号名，不是行号**。行号会漂，符号名相对稳。
> 括号里的数字只是「大概在这个文件的什么位置」，方便肉眼扫；
> 精确定位一律用 `grep -n "符号名" 文件`。

## 1. 仓库全景

| 目录 | 是什么 | 什么时候来这里 |
| --- | --- | --- |
| `crates/automation-core` | 编排核心：状态机、trait、匹配策略、比例换算、审计契约 | 改流程、改判据、加状态 |
| `crates/vision` | 视觉层：OCR 适配器、像素/裁切、模板匹配、证据脱敏 | 改识别、改匹配算法 |
| `crates/platform-windows` | 真实平台：Win32 截屏/焦点/鼠标/键盘/剪贴板 + **全局热键** + 只读探针 | 改系统动作、排查「鼠标不动」 |
| `crates/platform-mock` | 演练替身：五个端口的假实现 + 场景注入 | 加演练场景、改故障注入 |
| `crates/storage` | SQLite：审计、发送台账、证据落盘 | 改落库结构、改清理策略 |
| `apps/desktop/src-tauri` | **IPC 命令层 + 装配**：23 个命令、配置草稿/持久化、`build_runner` | 加界面按钮、加配置项（改这里最多） |
| `apps/desktop/src` | React 界面：配置面板、任务、图标库 | 改界面 |
| `tools/winocr` | 独立 OCR 子进程（`Windows.Media.Ocr`） | 改 OCR 输出格式或放大倍数 |
| `apps/webview-probe` | 独立渲染探针，**与主应用零代码依赖**，删掉不影响主应用 | 排查「界面出不来」 |

## 2. 依赖方向

实测（2026-09-18 逐个读 `Cargo.toml`）：

```text
                     automation-core            ← 零内部依赖（只有 thiserror/uuid/serde/sha2）
                     trait + 状态机 + 策略
                             ▲
        ┌────────────┬───────┴────────┬──────────────┐
        │            │                │              │
  platform-mock  platform-windows   vision        storage
   （演练替身）    （真实 Win32）    （OCR/模板）   （SQLite）
        └────────────┴────────────────┴──────────────┘
                             ▲
                    apps/desktop/src-tauri
                    IPC 命令层 + 装配 build_runner
                             │ invoke / listen
                    apps/desktop/src（React）

  tools/winocr         独立子进程，不依赖任何内部 crate，只走 stdin/stdout JSON
  apps/webview-probe   独立探针，与主应用无代码依赖
```

三个必须记住的点：

- `automation-core` **零内部依赖**。往它加依赖 = 破坏分层，先停下来想清楚。
- 四个实现 crate **互不依赖**，都只指向 core。`vision` 不依赖 `storage`，
  所以证据 PNG 由 `vision` 产出字节、由 `storage` 落盘，中间靠 core 的契约衔接。
- `platform-windows` 在 src-tauri 里是 **`[target.'cfg(windows)'.dependencies]`**
  （`apps/desktop/src-tauri/Cargo.toml:41`），不是普通依赖。改平台层时别在
  `[dependencies]` 里找不到就以为没引用。

## 3. 文件清单

### 3.1 `crates/automation-core` —— 编排核心

| 文件 | 行数 | 职责 | 关键符号 |
| --- | --- | --- | --- |
| `src/ports.rs` | 388 | **五个端口的 trait 定义** + `AutomationError` + `IconQuery`/`IconPrior` | `DesktopPlatform`、`LocalOcr`、`IconLocator`、`ContactMatcher`、`HumanConfirmation` |
| `src/state.rs` | 532 | 任务状态枚举与合法迁移 | `TaskState` |
| `src/runner/mod.rs` | 1224 | **编排骨架**：配置、端口、主循环、通用工具 | `WorkflowRunner`、`RunnerConfig`、`execute()` |
| `src/runner/search.rs` | 440 | **搜索式工作流**（工作流 3） | `search_contact_by_keyword`、`pick_contact_from_dropdown`、`open_chat_from_profile` |
| `src/runner/list.rs` | 261 | **列表扫描式**（原有那条路） | `locate_contact`、`sweep_contact_list`、`scroll_to_top` |
| `src/runner/navigate.rs` | 163 | 导航图标模板匹配 + 点击（工作流 1 / 2） | `navigate_to_view`、`nav_prior` |
| `src/runner/message.rs` | 131 | 聚焦输入框 + 逐字填正文 | `prepare_message` |
| `src/policy.rs` | 445 | `ContactMatcher` 的实现：逐字精确匹配 + 宽松开关 | `ExactNameMatcher` |
| `src/regions.rs` | 174 | 比例区域 → 像素矩形换算 | `RelativeRegion` |
| `src/audit.rs` | 185 | 审计契约（只存长度/哈希/时间） | `AuditSink`、`SendLedger` |
| `src/lib.rs` | 18 | 模块声明与 re-export | — |

⚠️ **`runner` 是个目录**（`runner/mod.rs` + 四个子模块）。四个子模块里各有一批
`impl Run<'_> { … }` 块，方法一律是 `pub(super) fn`——不是随手加的可见性，
而是**必须**：Rust 的私有意味着「只有定义它的模块及其子孙可见」，
搬进子模块后父模块里的 `execute()` 就调不到了。
统一加 `pub(super)` 而不是逐个判断「谁被父模块调用」——那种判断改一次调用点就过时。

`runner/mod.rs` 内部结构（最值得先认脸的部分，行号仅供扫读）：

| 符号 | 位置 | 干什么 |
| --- | --- | --- |
| `RunnerConfig` | 313 | 全部可调参数。**加配置项的终点站** |
| `Workflow` / `NavTarget` | 244 / 294 | 三条工作流与两个导航目标 |
| `RunnerPorts` | 583 | 五个端口打包在一起传给 runner |
| `WorkflowRunner::run` | 665 | 对外入口 |
| `execute()` | 1143 | **状态机主干**，一路串到 `Completed`。改流程基本都在这里 |
| `advance()` | 743 | 唯一的状态迁移出口（含审计 + 进度通知） |
| `settle()` | 795 | 错误收敛：决定转 `NeedsHumanReview` 还是 `Failed` |
| `enter_client()` | 1081 | 接管已就绪的窗口 + 标定尺寸校验 |
| `verify_candidate()` | 1236 | 核验命中的名字就是目标（两条路共用） |
| `verify_chat_header()` | 1259 | 核验聊天页标题（两条路共用） |
| `wait_for_settle()` | 882 | 滚完等画面连续两帧一致 |
| `with_retry()` | 907 | 唯一的重试入口（只有平台 I/O 与超时走这里） |
| `ensure_not_frozen()` | 978 | 卡死判据，**每个有副作用的动作前都要过** |
| `ensure_calibrated_size()` | 1024 | 标定尺寸/DPI 校验；不符则**自动调回**，调不动才转人工 |
| `capture_and_recognize()` | 949 | 截图 + OCR 的组合步 |

各子模块的入口（改哪条工作流就打开哪个）：

| 想改什么 | 打开 |
| --- | --- |
| 搜索框怎么点、联想下拉怎么挑人 | `runner/search.rs` 的 `search_contact_by_keyword` / `pick_contact_from_dropdown` |
| 资料页怎么核验、怎么滚到底、怎么点「发消息」 | `runner/search.rs` 的 `verify_profile` / `scroll_profile_to_bottom` / `open_chat_from_profile` |
| 会话列表怎么滚、滚完怎么判「到底了」 | `runner/list.rs` 的 `sweep_contact_list` / `view_moves_when_scrolling` |
| 导航图标匹配与位置先验 | `runner/navigate.rs` |
| 正文怎么填、在哪一步停下 | `runner/message.rs` 的 `prepare_message` |


### 3.2 `crates/vision` —— 视觉层

| 文件 | 行数 | 职责 | 关键符号 |
| --- | --- | --- | --- |
| `src/ocr.rs` | 242 | `ExternalOcr`：截图 → PNG → 子进程 stdin → JSON stdout | `ExternalOcr`、`UnconfiguredOcr` |
| `src/template.rs` | 868 | 纯 Rust NCC 模板匹配（等价 `TM_CCOEFF_NORMED`），逐通道平均 | 匹配函数 |
| `src/pixels.rs` | 274 | **BGRA / 自上而下**的像素约定、裁切、放大 | — |
| `src/evidence.rs` | 116 | 证据脱敏（每个文字框涂成中灰） | — |
| `src/lib.rs` | 74 | 铁律：只处理内存中的局部截图、禁止网络 | — |

### 3.3 `crates/platform-windows` —— 真实平台

| 文件 | 行数 | 职责 |
| --- | --- | --- |
| `src/winapi.rs` | 915 | Win32 原语：BitBlt 截屏、`SetForegroundWindow`、`SetCursorPos`、`SendInput`、剪贴板 |
| `src/hotkey.rs` | 319 | ★ **全局热键**：`RegisterHotKey` + 独立线程消息循环 + 注销。按键解析与虚拟键码换算也在这里 |
| `src/hotkey/tests.rs` | 165 | 按键解析、虚拟键码区间、修饰键位、失败文案（测试文件不限行数） |
| `src/desktop.rs` | 426 | `WindowsDesktop` 实现 `DesktopPlatform`（含 `is_responsive` → `IsHungAppWindow`） |
| `src/config.rs` | 82 | 平台侧配置 |
| `examples/screen_probe.rs` | 900 | **只读探针 CLI**，标定/排查全靠它（命令表见 `REFERENCE.md` §1） |

### 3.4 `crates/platform-mock` —— 演练替身

`desktop.rs`(315) 截屏与输入、`vision.rs`(122) 假 OCR、`icons.rs`(172) 假模板匹配、
`scenario.rs`(121) **场景定义（加演练场景来这里）**、`fault.rs`(62) 故障注入、`confirmation.rs`(66) 自动确认。

### 3.5 `crates/storage`

`db.rs`(74) 建表（**结构上就没有放正文的列**）、`store.rs`(271) 审计与发送台账、
`evidence.rs`(230) 证据落盘与清理、`time.rs`(35)。

### 3.6 `apps/desktop/src-tauri` —— 改动最频繁的地方

| 文件 | 行数 | 职责 |
| --- | --- | --- |
| `src/lib.rs` | 1853 | **23 个 IPC 命令** + `with_commands` 注册（含**托管状态**）+ 任务日志落盘 |
| `src/tests.rs` | 88 | `lib.rs` 的用例（测试文件不限行数） |
| `src/capture_hotkey.rs` | 138 | 「一键截屏」全局热键的接线：`attach` 挂托管状态 + 持有注册句柄 + 命中时只发事件（**不自己截图**，理由见文件头） |
| `src/runtime.rs` | 893 | `RuntimeConfig`（草稿与已保存）、`build_runner` 装配、模式校验、**按工作流列出所需标定区域** |
| `src/runtime/tests.rs` | 653 | 装配期校验的用例（哪些配置必须在登记任务之前就被拒） |
| `src/calibration.rs` | 370 | 标定清单的**数据结构 / 存取 / 视图组装**（清单内容在 `calibration/catalog.rs`） |
| `src/calibration/catalog.rs` | 198 | **清单本身**：4 个场景、14 个标定项，纯数据 |
| `src/calibration/tests.rs` | 287 | 清单内容与失效项清理的测试（测试文件不限行数） |
| `src/data_dir.rs` | 146 | ★ **数据目录在哪**（程序运行当前路径下的 `data/`）。**全项目唯一一处** |
| `src/legacy_data.rs` | 244 | 一次性把旧版留在 AppData 里的配置与图标库搬到数据目录 |
| `src/icon_library.rs` | 195 | 图标库对外接口 + 名字校验（实现在 `icon_library/`） |
| `src/icon_library/{layout,read,write}.rs` | 79 / 188 / 145 | 目录定位 / 目录 → 列表 / 存与删 |
| `src/icon_library/tests.rs` | 470 | 图标库测试（名字校验、定位、读写、坏文件） |
| `src/confirmation.rs` | 127 | `HumanConfirmation` 实现（oneshot 等待界面确认） |

**23 个 IPC 命令清单**（`lib.rs` 里搜 `#[tauri::command]`；
前端调用点见 `apps/desktop/src/api.ts`，**两边名字一一对应**）：

```text
start_task            list_tasks           get_task             confirm_task
cancel_task           runtime_info         set_runtime_config   preview_target_window
pick_target_window    launch_client        record_window_geometry  probe_nav_icon
list_icons            delete_icon          delete_icon_variant  save_icon_from_crop
click_icon            list_calibration_plan  validate_area_mark  prune_stale_marks
workflow_requirements register_capture_hotkey  unregister_capture_hotkey
```

最后两个在 `capture_hotkey.rs` 里（不在 `lib.rs`），注册句柄由 `HotkeyState` 托管。
热键命中时后端只发一条 `calibration://capture-hotkey` 事件，**不自己截图**——
截哪张图取决于界面正在标定哪个界面和配置草稿，只有界面知道。见
`docs/windows-mvp-interface.md`「截图的三种触发方式」。

★ **新增托管状态放哪**：不需要 `AppHandle` 的挂进 `with_commands`（热键就是这么做的，
落点是 `capture_hotkey::attach`），需要它的才挂 `run()` 的 setup。原因是 `Builder::setup`
**只在 `App::run()` 里执行**，集成测试的 `build()` 不触发它——挂 setup 就等于
「生产一处、测试一处」，靠人记得同步。`AppState` 挪不了（要读数据目录路径），
只能两边各注册一次。

`workflow_requirements` 是**只读**命令：回答「三条工作流各自要求先标哪几块区域、
现在标了没有」。它和装配期的判据是**同一份**（`runtime::required_marks`）——
界面**不许**自己列一张表，两边不一致时的表现是「界面说齐了、点开始却被拒」。
用例 `ipc_flow.rs::the_requirement_list_matches_what_assembly_enforces` 就是拦这个的。

`validate_area_mark` 是**纯校验命令**：只回答「这个标定框合不合法」，
不改任何状态。界面在拖完框松手时调它；配置仍走 `set_runtime_config`。
为什么不直接写配置见 `docs/todo.md` T12——界面的保存模型是草稿式的。

`prune_stale_marks` 是它的反面：**直接落盘**。它清掉的是「清单里已经没有、
但配置里还留着」的标定项，而那些项正是 `set_runtime_config` 会拒绝的东西——
只改内存的话用户下一次保存仍会被自己挡住。两条写入路径都走 `persist_config`，
所以「落到盘上的配置一定过了校验」只有一处实现。

只读辅助命令（`preview` / `pick` / `measure` / `probe` / `list`）**不按运行模式设限**，
别给它们加模式判断。

`lib.rs` 内部辅助函数：`persist_config` 校验 + 落盘 + 更新内存（两条写入路径共用）、
`migrate_legacy_data` 启动时搬一次旧数据、`desktop_for` 按类名+exe 造平台对象、
`preview_data_url` 截屏转 base64、`window_rect_from_preview` 预览坐标 → 窗口坐标、
`append_task_log` 任务日志落盘。

### 3.7 `apps/desktop/src` —— 界面

| 文件 | 行数 | 职责 |
| --- | --- | --- |
| `App.tsx` | 341 | **持有配置草稿 `draft`**（三个页签改同一份）+ 页签切换 |
| `api.ts` | 363 | 所有 `invoke` 封装 + 事件订阅。**要改命令名先看这里** |
| `types.ts` | 676 | 与 Rust 侧 serde 结构对应的类型（含 `Workflow` / `NavTarget` / `HotkeyRequest`） |
| `calibration.css` | 470 | 「界面标定」页与区域画布的样式（自成一套） |
| `components/RuntimePanel.tsx` | 782 | 整组运行配置表单（**配置项都显示，不按模式隐藏**）+ 启动搬迁提示。⚠️ 超基线，见 `docs/todo.md` T17 |
| `components/IconLibraryPanel.tsx` | 1064 | 图标库：截图 → 框选 → 命名 → 存模板 → 勾「用于导航」并选**导航目标**。⚠️ 超基线，见 T17 |
| `components/CalibrationPanel.tsx` | 722 | 界面标定页：左栏场景 + 项清单，右栏截图与画布；含失效项清理出口。⚠️ 超基线，见 T17 |
| `components/CaptureTrigger.tsx` | 297 | **截图的三种触发方式**：立刻 / 延时倒计时 / 全局热键。为什么必须有后两种见 `windows-mvp-interface.md` |
| `components/RegionCanvas.tsx` | 323 | 可拖拽画布：移动 / 八向缩放 / 空白处拉新框 |
| `components/RegionCalibration.tsx` | 284 | 四个核心区域的百分比表单（嵌在「任务」页里） |
| `components/WindowPicker.tsx` | 172 | 倒计时悬停取窗口 |
| `components/TaskForm.tsx` / `TaskHistory.tsx` / `StateTimeline.tsx` / `ConfirmationDialog.tsx` | 70 / 72 / 66 / 59 | 任务发起、历史、状态轨迹、人工确认 |

## 4. ★ 需求 → 文件（反向索引）

| 我要做的事 | 打开这些 |
| --- | --- |
| 改状态机流程 / 加一个状态 | `automation-core/src/state.rs` + `runner/mod.rs` 的 `execute()` + `apps/desktop/src/types.ts` + `components/StateTimeline.tsx` |
| 改联系人匹配规则（含宽松匹配） | `automation-core/src/policy.rs`。**判据只有这一处**，别在 runner 里另写 `==` |
| **改搜索式工作流**（工作流 3：点搜索框 → 下拉挑人 → 资料页 → 发消息） | `runner/search.rs`。资料页那一套（核验 / 滚到底 / 找入口）也在这里 |
| **改列表扫描式**（原有那条：滚会话列表认名字） | `runner/list.rs` |
| **改「只做导航」**（工作流 1 / 2） | `runner/navigate.rs`（判据与点击）+ `vision/src/template.rs`（匹配算法） |
| **改「在哪一步停下 / 要不要发」** | `runner/message.rs` 的 `prepare_message`。搜索式与「只填不发」都停在 `Prepared` |
| 改三条工作流各自要求先标哪些区域 | `src-tauri/src/runtime.rs` 的 `required_marks`。**判据只有这一处**，界面通过 `workflow_requirements` 命令读它 |
| 改区域比例默认值 / 加一个区域 | `automation-core/src/regions.rs` + `src-tauri/src/runtime.rs` 的 `RegionConfig` + `components/RegionCalibration.tsx` |
| 加一个**界面标定**要标的区域 / 改引导语 | `src-tauri/src/calibration/catalog.rs` 的 `ITEMS` / `SCENES`（**清单只有这一处**）+ `components/CalibrationPanel.tsx`。加一项**不必**动 Rust 结构体、TS 类型和界面元数据表 |
| 改标定框的校验规则（最小尺寸等） | `automation-core/src/regions.rs` 的 `RelativeRegion::validate`。**判据只有这一处**，`calibration::validate_rect` 与 `lib.rs` 都是转调它 |
| 改**数据目录**在哪（相对路径 → 别的规则） | `src-tauri/src/data_dir.rs`。**只有这一处**：图标库的默认位置（`icon_library/layout.rs`）也走它 |
| 改**图标库**默认位置 / 配置里相对路径怎么解析 | `src-tauri/src/icon_library/layout.rs`。基准取 `data_dir::working_dir()`，别在这儿另写 `current_dir()` |
| 改旧数据搬迁（搬哪些、什么条件搬） | `src-tauri/src/legacy_data.rs`。调用点在 `lib.rs` 的 `migrate_legacy_data`，**失败不阻断启动** |
| 改失效标定项的检测 / 清理 | 检测在 `calibration.rs` 的 `stale_keys` / `plan().stale_keys`；清理命令 `prune_stale_marks`（`lib.rs`）+ `api.ts` + `CalibrationPanel.tsx` 的提示条 |
| 改滚动行为（次数/落点/停稳） | 列表滚动 → `runner/list.rs`；资料页滚动 → `runner/search.rs`。**两套刻意不复用**，别合并 |
| 改导航图标匹配 | `vision/src/template.rs`（算法）+ `runner/navigate.rs`（判据与点击）+ `icon_library.rs`（模板存取） |
| 改 OCR 调用方式或输出解析 | `vision/src/ocr.rs`（契约与解析）+ `tools/winocr/src/main.rs`（实现） |
| 改 Win32 动作（点击/滚动/粘贴） | `platform-windows/src/desktop.rs`（契约实现）+ `winapi.rs`（原语） |
| 改**截图的触发方式**（延时秒数、热键界面） | `components/CaptureTrigger.tsx`（界面）→ `src-tauri/src/capture_hotkey.rs`（命令）→ `platform-windows/src/hotkey.rs`（注册与消息循环）。⚠️ 延时秒数**刻意写死**，理由在该组件顶部 |
| 改**全局热键允许哪些键** | `platform-windows/src/hotkey.rs` 的 `HotkeyKey`。**判据只有这一处**：界面故意用自由文本输入而**不列候选表**，就是为了不出现第二处 |
| 排查「按了热键没反应」 | 先看界面上「已生效：Ctrl+Alt+S」在不在（不在 = 没注册成功，错误文案就在旁边）；再看 `capture_hotkey.rs` 的事件是否发到了界面 |
| 排查「鼠标不动 / 点了没反应」 | 先看 `task-<ID前8位>.log`（`REFERENCE.md` §11.2 的排查顺序），再动 `winapi.rs` |
| 改审计字段 / 加发送台账 | `storage/src/{db,store}.rs` + `automation-core/src/audit.rs` |
| 改证据脱敏方式 | `vision/src/evidence.rs`（涂灰逻辑）+ `storage/src/evidence.rs`（落盘与清理） |
| 加演练场景 / 故障注入 | `platform-mock/src/scenario.rs` + `fault.rs` |
| 加界面按钮（会真的产生输入） | `lib.rs` 加命令 + `api.ts` + `App.tsx`/对应组件。**确认它该不该按运行模式设限** |
| 改窗口识别规则（类名/exe） | `lib.rs` 的 `desktop_for` + `platform-windows/src/config.rs` |

## 5. 跨层改动清单（最容易漏一处的那种）

**加一个配置字段 —— 4 处**，漏任何一处都会「界面能填但不生效」或「保存后被静默丢弃」：

1. `apps/desktop/src-tauri/src/runtime.rs` → `RuntimeConfig` 加字段（带 `#[serde(default)]`）
2. `apps/desktop/src/types.ts` → 同名字段（**注意 serde 的命名转换**）
3. `apps/desktop/src/components/RuntimePanel.tsx` → 表单控件
4. `apps/desktop/src-tauri/src/runtime.rs` 的 `build_runner` → 映射进 `RunnerConfig`
   （若该字段属于编排层，还要在 `automation-core/src/runner/mod.rs` 的 `RunnerConfig` 加一份）

⚠️ **第 4 步的落点不一定在 `RunnerConfig`。** 按字段属于哪一层决定：

| 字段属于 | 落点 | 例子 |
|---|---|---|
| 编排层（怎么走流程、什么时候停） | `RunnerConfig` 的字段，在 `to_runner_config` 里赋值 | `workflow`、`stop_before_send` |
| **平台层**（怎么操作鼠标键盘） | 在 `build_runner` 里构造 `WindowsDesktopConfig` 时传进去 | `typing_interval_ms` → `WindowsDesktopConfig.typing_interval` |
| **标定页标出来的区域** | **不建顶层字段**，走 `area_marks` 字典 + `mark_region(self, "<key>")` | `nav_bar` / `main_search` / `search_dropdown` / `contact_profile` |

第三种最容易看漏：字段名只出现在 `calibration/catalog.rs` 的 `ITEMS`（`key`）与
`mark_region` 的调用点，`RuntimeConfig` 里搜不到它——**但它是配置的一部分**，
界面上标了没标直接决定装配期拒不拒。

自查方法：把 `RunnerConfig` 的字段列表与 `to_runner_config` 里赋的那些对一遍，
**多出来的**就是没接线（会静默用默认值）；再把 `RuntimeConfig` 与 `types.ts` 对一遍，
**只有一边有的**就是漏同步。

**加一条工作流 —— 6 处**（新增于 2026-09-19，三条工作流那批改动）：

1. `automation-core/src/runner/mod.rs` → `Workflow` 枚举加一项 + `describe()` 加分支
2. 同文件的 `state.rs` 迁移表若需要新状态，先加状态（见下面那条）
3. 同文件的 `RunnerConfig::default()` 加默认值（**默认值要在常量里，不要写字面量**）
4. 同文件的 `execute()` 里加分支（放在 `Workflow::NavigateOnly` 之后那段 `match`）
5. `apps/desktop/src-tauri/src/runtime.rs` → `RuntimeConfig.workflow` 的
   **`required_marks`** 加一行（这条工作流要先标哪几块区域）。
   ⚠️ 这是**判据唯一的一处**，界面通过 `workflow_requirements` 命令读它，
   **不许**在 `RuntimePanel.tsx` 里另列一张表
6. `apps/desktop/src/types.ts` 的 `Workflow` 联合类型 + `RuntimePanel.tsx` 的
   `WORKFLOWS` / `WORKFLOW_LABELS` 下拉项

⚠️ 第 5 步漏了的表现是「界面说齐了、点开始却被拒」——装配期会用 `required_marks`
拒绝，而人只会去怀疑标定本身。`ipc_flow.rs::the_requirement_list_matches_what_assembly_enforces`
就是拦这个的。

**加一个 IPC 命令 —— 3 处**：`lib.rs` 写 `#[tauri::command]` → `with_commands` 里注册
（`lib.rs:1571` 的 `generate_handler!`，**漏了会在运行时才报「命令不存在」**）→ `api.ts` 加封装。

**加一个任务状态 —— 4 处**：`state.rs` 枚举 + 合法迁移表 → `execute()` 里的迁移 →
`types.ts` 的 `TaskState` 联合类型 → `StateTimeline.tsx` 的终态判断
（`TERMINAL_FAILURE` 数组，漏了界面会把终态画成进行中）。

**加一个区域 —— 3 处**：`regions.rs` 的比例定义 → `runtime.rs` 的 `RegionConfig`
→ `RegionCalibration.tsx` 的 `REGION_KEYS`（**只放键名**；中文名、编号、所属场景都在后端
`calibration/catalog.rs` 的 `ITEMS` 里，界面标定页直接用后端下发的值，别再抄一份）。

**加一个端口方法 —— 4 处**（`DesktopPlatform` 这类 trait 加方法，漏一处就编译不过，
但最容易漏的是最后一处「替身没跟上」）：

1. `crates/automation-core/src/ports.rs` → trait 加方法（**约定写在文档注释里**）
2. `crates/platform-windows/src/desktop.rs` → 真实实现
3. `crates/platform-mock/src/desktop.rs` → 替身实现
4. `crates/platform-mock/src/fault.rs` → `MockFaults` 加一格注入点

⚠️ 第 3 步不是"补一个 `Ok(())` 就完事"。替身要能表达真实实现里**所有**的失败形态，
否则用例只测得到顺利路径。`resize_wecom` 的 `min_window_size`（模拟客户端把尺寸夹住）
就是为此加的——"调用报成功、结果却不对"这种分支只能这么测。

## 6. 测试分布：改什么跑什么

| 测试文件 | 行数 | 管什么 | 跑法 |
| --- | --- | --- | --- |
| `crates/platform-mock/tests/mvp_flow.rs` | 2115 | **端到端全流程**（演练模式）。改状态机/流程必跑 | `cargo test -p platform-mock` |
| `crates/platform-windows/tests/windows_contract.rs` | 455 | 平台层契约 | `cargo test -p platform-windows` |
| `crates/platform-windows/src/hotkey/tests.rs` | 165 | 热键：按键解析（含 `F` 是字母、`F13` 非法）、虚拟键码区间、修饰键位、失败文案 | `cargo test -p platform-windows --lib hotkey` |
| `crates/storage/tests/persistence.rs` | 173 | 审计往返、正文不落库、幂等台账 | `cargo test -p storage` |
| `apps/desktop/src-tauri/tests/ipc_flow.rs` | 1547 | IPC 全链路（`MockRuntime`）+ **装配期该拒绝什么** + 热键命令的往返与校验 | `cargo test -p desktop` |
| `apps/desktop/src-tauri/tests/live_smoke.rs` | 508 | 实机冒烟 | 手动 |
| `apps/desktop/src-tauri/tests/live_wechat.rs` | 453 | **实机验证**（真实鼠标键盘，`#[ignore]`，强制 `stop_before_send`） | 见该文件头部注释 |

全量：`CARGO_INCREMENTAL=0 cargo test --workspace --no-fail-fast`
（`--no-fail-fast` 不能省，否则首个失败目标会中断整轮）。

被 `#[ignore]` 跳过的真机用例（会真的占系统热键 / 动真实鼠标键盘）：

```bash
cargo test -p platform-windows -- --ignored hotkey   # 注册 → 注销 → 再注册同一个组合
cargo test -p desktop --features custom-protocol -- --ignored a_hotkey_survives
```

## 7. 维护约定

- **写代码前先读根目录 `CONVENTIONS.md`**（规模上限、拆分与复用、错误处理、
  `unsafe` 边界、存量超标基线）。这份地图只回答「在哪」，不回答「怎么写」。
- **改了文件职责就顺手改这里的一行**。地图腐化比没有地图更坏——
  它会让新会话按错误的地图去读文件。
- **只写「在哪」，不写「为什么」**。为什么放 `architecture.md`，
  取舍放 `todo.md`，实测细节放 `REFERENCE.md`。写串了就会三处不一致。
- **不要往这里堆代码片段**。它的全部价值在于便宜——新会话读它一次就能定位。
  一旦它变成第二份 architecture.md，就没人读了。
