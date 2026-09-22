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
| `crates/platform-mock` | 演练替身：四个端口的假实现 + 场景注入 | 加演练场景、改故障注入 |
| `crates/storage` | SQLite：审计、发送台账、证据落盘 | 改落库结构、改清理策略 |
| `apps/desktop/src-tauri` | **IPC 命令层 + 装配**：26 个命令、配置草稿/持久化、`build_runner`、**启动日志** | 加界面按钮、加配置项（改这里最多） |
| `apps/desktop/src` | React 界面：配置面板、任务、图标库、**过程重放** | 改界面 |
| `tools/winocr` | 独立 OCR 子进程（`Windows.Media.Ocr`） | 改 OCR 输出格式或放大倍数 |
| `tools/macosocr` | 同一契约的 macOS 实现（`Vision` 框架，`--upscale`） | 在 Mac 上跑 OCR / 改放大倍数 |
| `tools/replay` | **离线重放事件流**：拿盘上那一帧重跑判据，回答「19 块里为什么没找到」；加 `--ocr` 还能把 `raw/` 那张干净图喂回本机引擎，回答「卡在哪一层」 | 排查「它为什么这么判」、改阈值试结论、判断是读错了还是判错了（`cargo run -p replay -- data/tasks/<任务ID> [--min-confidence 0.70] [--ocr auto] [--upscale 3]`） |
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

  tools/winocr / macosocr   独立子进程，不依赖任何内部 crate，只走 stdin/stdout JSON
  tools/replay         依赖 automation-core（判据本体）与 vision（引擎调用 + stdout 解析）
  apps/webview-probe   独立探针，与主应用无代码依赖
```

★ `tools/replay` 这一条是**判据只有一处**（`CONVENTIONS.md` §1.3）撑起来的：
它重跑的是 `automation-core` 里那个函数本身，所以「改个阈值结论会不会变」的答案
来自判据，而不是来自重新解释一遍盘上的记录。格式定义在 `apps/desktop` 那侧
（`task_diagnostics/events.rs`），重放工具只读不写。

★ **`tools/replay` 依赖 `vision` 一条要理解清**（2026-09-21 加真 OCR 时改的）：
它只用了 `vision::ocr::ExternalOcr`（起引擎 + 解析 stdout + `recognize_png`）。
理由同样是"只有一处"：重跑出来的文字块必须与**盘上那份可比**，而盘上那份也是
`vision::parse_ocr_json` 解出来的——自己再写一套解析，两次读数的差异里就会混进
"两个解析器的差异"，那份报告也就不可信了。它**仍不依赖** `storage` /
`platform-*` / `apps/desktop`。

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
| `src/ports.rs` | 497 | **四个端口的 trait 定义** + `AutomationError` + `IconQuery`/`IconPrior` | `DesktopPlatform`、`LocalOcr`、`IconLocator`、`ContactMatcher` |
| `src/state.rs` | 277 | 任务状态枚举与合法迁移 | `TaskState` |
| `src/runner/mod.rs` | 1533 | **编排骨架**：配置、端口、主循环、通用工具。★ 单步超时（`check_deadline`）与**分段耗时证据**（`note_elapsed`）也在这里：超时文案必须带**实际耗时 + 判据自己的代码位置**，否则"这一步为什么慢"没有入口 | `WorkflowRunner`、`RunnerConfig`、`execute()`、`check_deadline`、`note_elapsed` |
| `src/runner/search.rs` | 492 | **搜索式工作流**（工作流 3）。★ 点下拉那一行之后**两种落点**：通常在资料页，已有会话时直接进聊天——由 `ProfileReview` 把"资料页上没认到"交回 `open_chat_from_dropdown` 定夺（见 `docs/todo.md` T32） | `search_contact_by_keyword`、`pick_contact_from_dropdown`、`verify_profile`、`open_chat_from_profile`、`open_chat_from_dropdown` |
| `src/runner/list.rs` | 261 | **列表扫描式**（原有那条路） | `locate_contact`、`sweep_contact_list`、`scroll_to_top` |
| `src/runner/navigate.rs` | 238 | 导航图标模板匹配 + 点击（工作流 1 / 2）。★ 每一步都记一条分段耗时（`timed!` 宏 + `Run::note_elapsed`）：截屏 / `icons.locate` / 诊断上报 / 守卫 / 点击各一段，`file:line` 取在宏展开处 | `navigate_to_view`、`nav_prior` |
| `src/runner/message.rs` | 246 | 聚焦输入框 + 把正文**逐字**敲进输入框，以及**发不发**这一步：勾了「只填不发」停在 `Prepared`，没勾就直接**点发送按钮** → 核验送达，**中间不插任何等人工的环节**。★ 判据只有 `stop_before_send` 一处，与工作流无关。★ 发送动作 = 逐字输入 + 点按钮：**不粘贴**（不碰剪贴板）、**不按 Enter**（客户端的发送键设置因人而异，按错键表现为"多了个换行、消息没出去"） | `prepare_message`、`type_into_composer`、`click_send_button`、`verify_delivery` |
| `src/policy.rs` | 445 | `ContactMatcher` 的实现：逐字精确匹配 + 宽松开关 | `ExactNameMatcher` |
| `src/policy/trail.rs` | 369 | ★★ **姓名匹配的判据本体**：结论（选中谁）与轨迹（每块为什么）**同一次交出** | `strict_judge`、`contains_judge`、`name_match_decision` |
| `src/policy/trail/verdicts.rs` | 176 | 轨迹的**措辞**层：逐块写「过 / 淘汰 + 理由」 | `verdicts_strict`、`verdicts_relaxed` |
| `src/dropdown.rs` | 469 | ★★ **搜索下拉挑人**这条判据（多分组 + 多命中取最上面），同样一次交出结论与轨迹 | `judge_dropdown`、`parse_search_contact_group_labels` |
| `src/diagnostics.rs` | 236 | ★★ **诊断契约**：一条轨迹长什么样（这块文字过没过 + 理由）、一次判定怎么记（判据名 / 阈值 / 候选 / 重跑参数）、一步「看到了什么」（区域 + 那一帧 + **整窗底图** + **图标命中**）。★ `WindowShot` 是"区域落在窗口哪儿"的唯一依据（只给裁图时区域框恒等于图像边界），取不到就退回裁图——诊断**永远不该**让任务失败 | `Observation`、`WindowShot`、`IconHit`、`Verdict`、`MatchTrail`、`Decision`、`ReplayInput`、`DiagnosticRecorder` |
| `src/regions.rs` | 174 | 比例区域 → 像素矩形换算 | `RelativeRegion` |
| `src/audit.rs` | 189 | 审计契约（只存长度/哈希/时间） | `AuditSink`、`SendLedger` |
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
| `Workflow` | 244 | 三条工作流（**导航目标不再是枚举**：见 `RunnerConfig::nav_target_label`） |
| `RunnerPorts` | 591 | 四个端口打包在一起传给 runner |
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
| 点完下拉那一行**没**落在资料页怎么办（对方已有会话，客户端直接开聊天） | `runner/search.rs` 的 `open_chat_from_dropdown` + `ProfileReview`。**顺序不能反过来**：必须先核验资料页，只有它报"没认到"才拿聊天标题定论（两块区域只差 2 像素），见 `docs/todo.md` T32 |
| 会话列表怎么滚、滚完怎么判「到底了」 | `runner/list.rs` 的 `sweep_contact_list` / `view_moves_when_scrolling` |
| 导航图标匹配与位置先验 | `runner/navigate.rs` |
| 正文怎么填、在哪一步停下 | `runner/message.rs` 的 `prepare_message` |


### 3.2 `crates/vision` —— 视觉层

| 文件 | 行数 | 职责 | 关键符号 |
| --- | --- | --- | --- |
| `src/ocr.rs` | 273 | `ExternalOcr`：截图 → PNG → 子进程 stdin → JSON stdout。★ `recognize_png` 吃的就是**那串 PNG 字节**（`raw/NN-*.png` 存的就是它）——离线重跑要原样喂回去，重新解码再编码等于给"跑的是不是当时那张图"多加一道可疑环节 | `ExternalOcr`、`UnconfiguredOcr` |
| `src/template.rs` | 904 | 纯 Rust NCC 模板匹配（等价 `TM_CCOEFF_NORMED`），逐通道平均。★ **分数不够时**再报"每个模板的前 3 个候选"（`top_candidates` / `candidate_report`）—— 只报最高分分不出「模板对不上画面」和「搜索区里根本没有它」这两件事。★★ **开销 = 搜索位置数 × 模板像素数**：每张模板都要把搜索区**逐位置扫一遍**，所以慢的原因几乎总是「搜索区太大 / 模板太大 / 模板太多 / debug 构建」，不是阈值（见 `docs/todo.md` T31）。⚠️ 七档金字塔（`TEMPLATE_SCALE_PYRAMID`）**不在主路径上**（`locate` 是单尺度），它那几个函数目前是 dead code | 匹配函数、失败诊断 |
| `src/pixels.rs` | 274 | **BGRA / 自上而下**的像素约定、裁切、放大 | — |
| `src/evidence.rs` | 116 | 证据脱敏（每个文字框涂成中灰） | — |
| `src/render.rs` | 375 | ★★ **过程诊断图的渲染**：`annotate_step` 一页（底图 + 三类框 + 右侧栏）+ `compose_overview` 总图。★ 底图优先**整窗**（拿不到才退回裁图）；橙框 = 要识别的区域、绿框 = 文字块、蓝框 = 图标命中。★ 坐标换算只在这一处：**屏幕坐标 / 帧图像坐标 → 页面坐标**（Retina 上帧是物理像素，先除回逻辑点、再乘底图比例）。用例在 `render/tests.rs`（主体已贴行数上限） | `annotate_step`、`compose_overview`、`Step` |
| `src/lib.rs` | 74 | 铁律：只处理内存中的局部截图、禁止网络 | — |

### 3.3 `crates/platform-windows` —— 真实平台

| 文件 | 行数 | 职责 |
| --- | --- | --- |
| `src/winapi.rs` | 796 | Win32 原语：BitBlt 截屏、窗口枚举/定位、`SetForegroundWindow`、剪贴板、进程启动、显示器指标。★ 光标轨迹与输入原语已搬出（此处只留 `pub use` 再导出，`winapi::move_cursor` / `winapi::left_click` 等路径不变） |
| `src/winapi/cursor.rs` | 324 | ★ **光标轨迹**：`move_cursor`（smoothstep 缓动）/ `move_cursor_circle`（**走完一圈再多走 1/4 圈，终点不回起点**）/ `circle_points`（纯计算，按 `turns` 圈扫过）+ `move_cursor_absolute` / `mouse_move_input`（`SendInput` 绝对坐标）。**不用 `SetCursorPos`、不注入随机抖动** —— 理由写在文件头 |
| `src/winapi/input.rs` | 219 | ★ **输入原语**：`key_input` / `mouse_input(_with)` / `scroll_wheel`（逐格发）/ `left_click` / `send_ctrl_*` / `send_delete` / `send_unicode_text`（按 UTF-16 码元逐字发）。**一律走 `SendInput`**。`mouse_move_input` 是 `pub(super)`：只给同级的 `cursor.rs` 用。⚠️ 曾经还有个 `send_enter`（回车发送），2026-09-22 换成「点发送按钮」后删掉 |
| `src/winapi/tests.rs` | 87 | 圆周点的**纯计算**用例（**不碰光标**：会劫持鼠标的用例比没有用例更糟）。⚠️ 它 `use super::cursor::{...}`，搬东西时别忘了改这里 |
| `src/hotkey.rs` | 319 | ★ **全局热键**：`RegisterHotKey` + 独立线程消息循环 + 注销。按键解析与虚拟键码换算也在这里 |
| `src/hotkey/tests.rs` | 165 | 按键解析、虚拟键码区间、修饰键位、失败文案（测试文件不限行数） |
| `src/desktop.rs` | 426 | `WindowsDesktop` 实现 `DesktopPlatform`（含 `is_responsive` → `IsHungAppWindow`） |
| `src/config.rs` | 82 | 平台侧配置 |
| `examples/screen_probe.rs` | 953 | **只读探针 CLI**，标定/排查全靠它（命令表见 `REFERENCE.md` §1）。★ **`findicon` 是"模板分数为什么低"最快的判据**：只读、不跑任务、几秒出结果（T28 就是靠它定的案） |

### 3.4 `crates/platform-mock` —— 演练替身

`desktop.rs`(315) 截屏与输入、`vision.rs`(122) 假 OCR、`icons.rs`(172) 假模板匹配、
`scenario.rs`(121) **场景定义（加演练场景来这里）**、`fault.rs`(62) 故障注入。
（原来还有 `confirmation.rs`「自动确认」——2026-09-22 随人工确认一起删了，见 `docs/architecture.md` §5 端口一节。）

### 3.5 `crates/storage`

`db.rs`(74) 建表（**结构上就没有放正文的列**）、`store.rs`(271) 审计与发送台账、
`evidence.rs`(230) 证据落盘与清理、`time.rs`(35)。

### 3.6 `apps/desktop/src-tauri` —— 改动最频繁的地方

| 文件 | 行数 | 职责 |
| --- | --- | --- |
| `src/lib.rs` | 2219 | **27 个 IPC 命令** + `with_commands` 注册（含**托管状态**）。★ 任务日志**不在这里**了，见 `task_log.rs`；过程事件流的读盘在 `task_replay.rs`。⚠️ **已破 `CONVENTIONS.md` §9 的存量基线（1903），见那一节的 2026-09-21 复测** |
| `src/task_log.rs` | 445 | ★ **任务日志的三件事**：`write_task_log_line`（纯追加、带毫秒时间戳、写完 flush）+ **`log_line!` 宏**（把**调用点的文件 / 行号 / 模块**自动挂上——位置只有取在宏展开处才是真的，靠人维护"文案 → 代码位置"必然静默出错）+ `write_start_header`（开跑前那份**配置快照**——"当时到底按哪份配置跑的"）。★ 快照里的**运行参数**（模式 / 工作流 / 导航目标）全部取自 `RunChoice`，与界面上选的必须一致。★ 读盘那侧：`summary_from_log` 用 `strip_line_prefixes` **循环剥行首前缀**（时间戳 + 代码出处两段都要剥，只剥一段会让每条 `strip_prefix` 落空、历史任务全变成"失败"）。T27 从 `lib.rs` 整段搬出来（那一百来行是"写日志"的活，与"装配 / 登记 / 起线程"不是一回事）；单测搬去了 `task_log/tests.rs` |
| `src/task_diagnostics.rs` | 315 | ★★ **过程诊断的落盘侧**（T29）：每个任务一个目录 `data/tasks/<ID>/`，逐步存标注图 + 总图 + `events.jsonl` + 人读的 `task.log` + **`raw/`（未标注的 OCR 输入图 + 引擎 stdout 原文）**。★★ **判据执行到哪就记到哪**：记的是判据**自己**交出来的轨迹，不是在编排层再复述一遍（否则迟早和判据不一致）。★ `observe` / `finish` 各自报一条 ⏱ 耗时（渲染+编码 / 写盘 / 总图拼装）：这段跑在**任务线程**上，每一毫秒都吃单步预算 |
| `src/task_diagnostics/events.rs` | 198 | ★★ **`events.jsonl` 的格式（唯一一处定义）**：`read`（这一步看到了什么：图 + 区域 + 每块文字与坐标与置信度 + **可重跑的原料 `ocr_input`/`ocr_raw`**）+ `decision`（这一步怎么判的：判据 / 阈值 / 每个候选过没过 + 理由 / 重跑要的参数）。`tools/replay` 只**读**这个格式 |
| `src/task_diagnostics/page.rs` | 79 | 把这一步的图 + 识别框 + 区域渲染成一张**标注图**（排查时先看它）。★ 底图优先用**整窗**（`observation.window`），拿不到才退回裁图；图标命中画成**蓝框**（与文字框的绿框区分），搜索区域画成橙框 | `render_page` |
| `src/task_replay.rs` | 171 | ★★ **「过程重放」的读盘侧**（T29）：读出事件流给界面，并把图补成**绝对路径**（asset 协议按目录前缀匹配，路径里的分隔符不能让前端猜）。★ `task_dir_for` 是**任务目录在哪的唯一判据**（界面「打开任务目录」按钮也走它）。⚠️ 旧布局的任务只有日志、没有目录 ⇒ 如实报"没有重放材料"，不拿空面板冒充 |
| `src/cursor_trace.rs` | 141 | ★ **「画圆」自检**（`draw_cursor_circle` + `CircleTraceView`）。★ 回报里带 **`end` / `end_distance_px`**（走完之后**实测**的光标位置与它到圆心的距离）—— "命令返回了"不等于"光标真的动了"，这是唯一能分辨的判据。单独成模块是为了让 `lib.rs` 不破基线；命令**仍注册在 `lib.rs`**。⚠️ 跨模块的命令必须是 `pub`（私有 ⇒ `E0603: macro import ... is private`）。见 `docs/todo.md` T24 |
| `src/startup_log.rs` | 165 | ★★ **启动期落文件日志**（`data/startup.log`）。GUI 没有控制台，而"窗口还没出现就退出"这类故障的现场**全在窗口出现之前**——这是唯一的现场。第一行截断、之后追加；**写失败一律静默**（观测手段不该成为新的失败点）；数据目录建不出来时**退回工作目录**记。另装了一个 panic 钩子（**包一层原钩子，不替换**）。见 `docs/todo.md` T25 |
| `src/tests.rs` | 88 | `lib.rs` 的用例（测试文件不限行数） |
| `src/capture_hotkey.rs` | 138 | 「一键截屏」全局热键的接线：`attach` 挂托管状态 + 持有注册句柄 + 命中时只发事件（**不自己截图**，理由见文件头） |
| `src/runtime.rs` | 927 | `RuntimeConfig`（草稿与已保存）、**`RunChoice`（本次怎么跑：模式 + 工作流 + 导航目标，运行参数）**、`build_runner` 装配、模式校验。★★ **`RunChoice` 的字段一律由请求带**，`build_runner` 的**第一件事**就是把 `config.mode` 换成请求里的那个，让后面所有"模式相关"的判断仍然只读 `config.mode`（判据只有一处）。见 `docs/todo.md` T23 |
| `src/runtime/requirements.rs` | 171 | ★★ **「这条工作流到底要什么」**：`required_marks`（必须标好哪几块区域）、`workflow_inputs`（要不要填「外部联系人名称」/「消息正文」）、`WorkflowRequirement`（下发给界面的那一份）。**装配期与界面共用同一份判据**——分叉的表现是「界面说齐了、点开始却被拒」或者**「按钮点不动、也不说为什么」**（T27 实测过后者）。T27 从 `runtime.rs` 搬出来 |
| `src/runtime/mode.rs` | 127 | ★ 「演练还是真实」这件事本身：`RuntimeMode`（+ `platform_label` / `calibrated_window` / `notice`）、`DemoScenario`、`ModeNotices`（两种模式各自的提示文案，成对下发）。**纯数据 + 纯函数**，自成一类，从 `runtime.rs` / `lib.rs` 搬出来压回行数基线。⚠️ 父模块有 `pub use mode::{…};` —— 少了它是一堆 `E0432: unresolved import`，看着像"文件没编进去" |
| `src/runtime/tests.rs` | 842 | 装配期校验的用例（哪些配置必须在登记任务之前就被拒） |
| `src/calibration.rs` | 370 | 标定清单的**数据结构 / 存取 / 视图组装**（清单内容在 `calibration/catalog.rs`） |
| `src/calibration/catalog.rs` | 198 | **清单本身**：4 个场景、14 个标定项，纯数据 |
| `src/calibration/tests.rs` | 287 | 清单内容与失效项清理的测试（测试文件不限行数） |
| `src/data_dir.rs` | 146 | ★ **数据目录在哪**（程序运行当前路径下的 `data/`）。**全项目唯一一处** |
| `src/legacy_data.rs` | 244 | 一次性把旧版留在 AppData 里的配置与图标库搬到数据目录 |
| `src/icon_library.rs` | 195 | 图标库对外接口 + 名字校验（实现在 `icon_library/`） |
| `src/icon_library/{layout,read,write}.rs` | 79 / 188 / 145 | 目录定位 / 目录 → 列表 / 存与删 |
| `src/icon_library/tests.rs` | 470 | 图标库测试（名字校验、定位、读写、坏文件） |

**26 个 IPC 命令清单**（`lib.rs` 里搜 `#[tauri::command]`；
前端调用点见 `apps/desktop/src/api.ts`，**两边名字一一对应**）：

```text
start_task            list_tasks           get_task             list_calibration_plan
cancel_task           runtime_info         set_runtime_config   preview_target_window
pick_target_window    launch_client        record_window_geometry  probe_nav_icon
list_icons            delete_icon          delete_icon_variant  save_icon_from_crop
click_icon            validate_area_mark   prune_stale_marks    workflow_requirements
register_capture_hotkey  unregister_capture_hotkey  draw_cursor_circle
read_task_log         read_task_events     open_task_dir
```

`read_task_log` 与 `read_task_events` 是一对（T29）：前一个给的是**给人读**的叙述，
后一个给的是**每一步看到什么、怎么判的**（`task_replay.rs`）。

`open_task_dir` 是「过程重放」页上那个按钮：在系统文件管理器里打开任务目录，
材料就摊在那儿（`raw/` 那张干净图、「拖给 `tools/replay` 的 `events.jsonl`」）。
★ 它走 **Rust 侧的 `tauri-plugin-opener`**（`app.opener().reveal_item_in_dir`），
不是前端 JS 那套插件 API——JS 调插件命令要在 capability 里放行，
而这里只需要打开一个后端自己算出来的路径（插件已在 `run()` 里 `init`）。
⚠️ 旧布局的任务没有目录时**报错**，不打开 `data/`。

`register_capture_hotkey` / `unregister_capture_hotkey` 这两个在 `capture_hotkey.rs` 里
（不在 `lib.rs`），注册句柄由 `HotkeyState` 托管。
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
`preview_data_url` 截屏转 base64、`window_rect_from_preview` 预览坐标 → 窗口坐标。
任务日志的落盘**不在这里**（`task_log.rs` 的 `write_task_log_line` / `log_line!`）。

### 3.7 `apps/desktop/src` —— 界面

| 文件 | 行数 | 职责 |
| --- | --- | --- |
| `App.tsx` | 628 | **持有配置草稿 `draft`**（几个页签改同一份）+ **持有运行参数 `runChoice`**（不进草稿、不置 `dirty`）+ **持有 `requirement`**（当前工作流要不要填联系人/正文、还缺哪块标定）+ 页签切换（含 T29 的「过程重放」页签与 `openReplay`）。★ `requirement` 放这里是因为它有**两个**消费者（`TaskForm` 与 `RunChoiceFields`），各取一次会有两份可能不同步的答案。⚠️ 超 500 行且没记过基线，见 T17 |
| `api.ts` | 428 | 所有 `invoke` 封装 + 事件订阅。**要改命令名先看这里** |
| `types.ts` | 794 | 与 Rust 侧 serde 结构对应的类型（含 `Workflow` / **`RunChoice`** / `StartTaskRequest` / `TaskFormValues` / `CircleTraceView` / `HotkeyRequest`）。★ `RunChoice.nav_target` 是**图标库里的目录名**（`string`），**不是**枚举——见 `docs/todo.md` T26。★ `WorkflowRequirement` 带 `needs_contact` / `needs_message`（T27） |
| `taskDisplay.ts` | 18 | `contactLabel`：任务列表与「当前任务」两处都显示收件人，空名字**不是"漏填了"**（「只做导航」根本不找人），所以给它一句话。**只写一处**，否则改文案必漏一处 |
| `countdown.ts` | 92 | ★ **「先倒数、到点再动手」的唯一实现**（`useCountdown` / `COUNTDOWN_TICK_MS` / `remainingSeconds`）。**到点判据是「现在 ≥ 截止时刻」**，别在任何调用点改成"把间隔累加起来"。秒数留在各调用点（截图 8 秒 / 画圆 3 秒） |
| `calibration.css` | 470 | 「界面标定」页与区域画布的样式（自成一套） |
| `components/RuntimePanel.tsx` | 291 | 「任务」页配置面板的**骨架**：页头三条提示（模式说明 / 未保存警告 / 搬迁结果）+ 四个开关 + 置信度 + 保存按钮 + 数据目录说明。★ **表单按"归属"拆给了四个同级组件**，见下面四行；加东西前先问"它属于哪一组"。★ `requirement` 只是**透传**（`App` → 这里 → `RunChoiceFields`），别在这里自己取 |
| `components/RunChoiceFields.tsx` | 341 | ★★ **运行参数那一组**：模式 / 演练场景 / 工作流 / 要点哪个图标 / 还缺哪块标定。**绑 `runChoice` 而不是 `draft`**（选完直接点开始就生效，不用保存）。它拿 `draft` 只剩**一个**用途：演练场景（唯一一个配置字段）。★ 「要点哪一个图标」的下拉**读图标库**（`listIcons()`，切页签会重新挂载所以自动刷新），列的是 `data/icons/` 的一级目录——**不要改回一份写死的清单**（见 T26）。★ `requirement` 由 `App` 传下来，**不要在这里自己问后端**（T27：两个消费者各问一次会有两份答案）。⚠️ 判据一句话：**要"保存配置"的留在 `RuntimePanel`，只管"这一次怎么跑"的搬这里** |
| `components/TargetWindowSection.tsx` | 202 | **目标窗口**那一组：指认窗口 / 可执行文件路径与哈希 / 启动客户端 / 窗口类名 / 记录窗口尺寸 / OCR 程序路径。自带「启动中」「读取中」两份局部状态。★ 全是**配置**，要保存 |
| `components/AdvancedParamsSection.tsx` | 192 | **超时 + 滚动查找**两组旋钮（含 `ratioFromInput`：清空输入框时**保留原值**，别退回 0）。平时不动，机器慢/列表长才来调 |
| `components/TypingTextSection.tsx` | 87 | **逐字输入间隔 + 靶标文字**（资料页入口文字、搜索下拉「联系人」分组标题）。这几个值**随客户端版本变**，改动理由相同，所以归到一起 |
| `components/IconLibraryPanel.tsx` | 800 | 图标库页**主面板**：截一张窗口图 → 在图上框住图标 → 起名字存下来（一个名字可存多张变体）；再加匹配参数与保存按钮。★ 列表与结果预览已拆出，见下面两行 |
| `components/IconList.tsx` | 204 | 图标库**列表**：一行一个图标 + 全部变体缩略图 + 「用于联系人导航」勾选 + 测试匹配 / 定位并点击 / 删除。★ 它**自带两个"两下确认"状态**（`armed` / `armDelete`）—— 主面板不必知道"哪个按钮上了膛"。⚠️ 「用于聊天历史导航」那个勾选框**已删**（T26）：它的旧用途（给写死的 `NavTarget::History` 喂模板）不存在了 |
| `components/NavResultSections.tsx` | 140 | 「匹配结果」「点击结果」两段**只读预览**（含 `ShotOverlay`：整窗图 + 搜索区蓝虚线 + 命中框 + 点击点）。★ 纯展示，**不判断结果对不对**——那句话是后端 `probe.notice` / `click.notice` 下发的 |
| `iconNames.ts` | 15 | ★ **`normalizeName`（图标名的比较键）**。它是**判据**（"算不算同一个图标"），只能有一处 —— 主面板与 `IconList` 都从这里取 |
| `replayView.ts` | 101 | ★★ **事件流 → "一步一步"的配对逻辑**（T29）：一步 = 一次「看图」+ 它后面**最近一次同名、还没被配走**的「判定」。★★ **只写这一处**（组件里再写一遍的症状是「图上这一步、表里是另一步的结论」，而两边都是真实数据，看不出错）。★ 盘上/工具侧的同一条规则在 `tools/replay/src/lib.rs::run`，改一处要看另一处 |
| `components/TaskReplay.tsx` | 285 | ★★ **「过程重放」页**（T29）：左图右表 —— 左边是那一步**当时的画面**（`convertFileSrc` + 后端补好的绝对路径），右边是**为什么这么判**（结论 + 每个候选过没过 + 逐条理由）。默认停在**失败那一步**。★ 「材料目录」旁边有**「打开任务目录」按钮**（`openTaskDir`）：材料（`raw/` 那张干净图、`events.jsonl`）都摊在那儿，不然人得自己拼 UUID 路径。⚠️ 图取不到就显示"这一步没有图"，不要让整页塌掉 |
| `components/CalibrationPanel.tsx` | 722 | 界面标定页：左栏场景 + 项清单，右栏截图与画布；含失效项清理出口。⚠️ 超基线，见 T17 |
| `components/CaptureTrigger.tsx` | 280 | **截图的三种触发方式**：立刻 / 延时倒计时 / 全局热键。倒计时走 `countdown.ts`。为什么必须有后两种见 `windows-mvp-interface.md` |
| `components/CursorMotionPanel.tsx` | 162 | 「轨迹自检」页：点按钮 → 倒计时 3 秒 → 光标画一圈（**画完不回起点**）。半径 = 本程序窗口**短边**的一半。★ 结果里那行「实测终点 · 离圆心 N 像素」是**唯一**能分辨"它到底动没动"的东西（走对了 N ≈ 半径，N≈0 就是没动）。见 `docs/todo.md` T24 |
| `components/RegionCanvas.tsx` | 323 | 可拖拽画布：移动 / 八向缩放 / 空白处拉新框 |
| `components/RegionCalibration.tsx` | 83 | ★ **不再是组件**：只剩 `DEFAULT_REGIONS`（四块区域的默认比例，后端是单一来源、这里只能手抄）与 `regionsAreValid`（保存前校验）。四个区域**只在「界面标定」页标**，旧的手填数字面板已删 |
| `components/WindowPicker.tsx` | 172 | 倒计时悬停取窗口。⚠️ **它不用 `countdown.ts`**：那个定时器每跳一次还要顺便采一次"光标下是哪个窗口"，数与采必须同一拍 |
| `components/TaskForm.tsx` / `TaskHistory.tsx` / `StateTimeline.tsx` | 121 / 95 / 86 | 任务发起、任务历史、状态轨迹。`TaskForm` **不碰「走哪条路」**，那是 `App.handleStart` 补进请求的。★★ `TaskForm` 按 `needsContact` / `needsMessage`（**后端下发**，T27）决定那两个框显不显示、按钮能不能点；⚠️ 这两个值是 `boolean \| null`，**`null`（还没问到）一律放行**——拒绝是看得见的，点不动是看不见的。★ `TaskHistory` 每一条底下有「过程重放」入口（T29，`onReplay` → `App.openReplay`）；它是 `div` 套按钮，**不是按钮套按钮**（HTML 会被浏览器拆开）。★ 界面里**没有**确认弹窗（2026-09-22 移除）：发不发只看「只填不发」 |

### 3.8 `tools/` —— 仓库外挂的小工具（不在 workspace 依赖链上）

| 文件 | 行数 | 职责 |
| --- | --- | --- |
| `replay/src/lib.rs` | 410 | ★★ **重放本体**：读 `events.jsonl` → 配帧（**与界面同一条配对规则**：同名、最近一次、还没被配走）→ 重跑判据 → 可选真 OCR → 归因。入口 `replay_dir_with_ocr` |
| `replay/src/ocr.rs` | 213 | ★★ **真 OCR 重跑**：`Engine`（发现 / 起进程 / 喂 `raw/` 那张干净图）、`diff_boxes`（两次读数的差异）。★ 配对是**文字相同里挑最近的**，不是按序号（同一句话会出现多次） |
| `replay/src/layer.rs` | 173 | ★★ **归因到层**：`attribute`（卡在 OCR / 判据 / 坐标 / 分不出）+ **「沾边」这条规矩的唯一实现**（`needles` / `touches_text`，报告也用它） |
| `replay/src/report.rs` | 268 | 把结果排成**给人看**的文本。★ 归因写在最前面、结论单列一行、差异逐条列（上限 `DIFF_LIMIT`）。⚠️ 输出是终端纯文本，**不许出现 Markdown 加粗**（`CONVENTIONS.md` §8） |
| `replay/src/main.rs` | 129 | 手写参数解析（`--min-confidence` / `--ocr` / `--upscale`）。★ 没找到引擎时**只提醒不报错**：判据重跑本身仍然有用 |
| `replay/src/tests.rs` | 360 | 重放回归 + **现场 fixture 的生成器**（`#[ignore]`，改判据措辞后重跑它一次） |
| `replay/src/ocr_tests.rs` | 295 | ★ 真 OCR 那几条（**假引擎 `sh` 脚本**，只造临时目录与假 PNG，不依赖现场素材）。⚠️ `#[cfg(all(test, unix))]` |
| `macosocr/macosocr.swift` | — | macOS 侧引擎（`Vision`）。契约与 `tools/winocr` 相同：PNG 走 stdin、JSON 数组走 stdout |

## 4. ★ 需求 → 文件（反向索引）

| 我要做的事 | 打开这些 |
| --- | --- |
| 改状态机流程 / 加一个状态 | `automation-core/src/state.rs` + `runner/mod.rs` 的 `execute()` + `apps/desktop/src/types.ts` + `components/StateTimeline.tsx` |
| 改联系人匹配规则（含宽松匹配） | `automation-core/src/policy.rs`（匹配器："什么算逐字相等"）+ **`policy/trail.rs`**（**判据本体**，结论与轨迹同源）。**判据只有这一处**，别在 runner 里另写 `==`。★ 改完顺手跑 `cargo run -p replay`，看旧现场上结论变没变 |
| 改搜索下拉怎么挑人（分组 / 阈值 / 多命中取哪一行） | `automation-core/src/dropdown.rs` 的 `judge_dropdown`。**判据只有这一处**，`runner/search.rs` 只调它 |
| **排查「判定为什么这么判」**（同一屏、同一份 OCR，为什么它没找到） | ① 界面「过程重放」页（`TaskReplay.tsx`，默认停在失败那一步：左图右表；旁边有「打开任务目录」按钮）；② `data/tasks/<任务ID>/task.log`（人读的叙述）+ `events.jsonl`（逐步的看到了什么 / 怎么判的）+ `steps/*.png`（标注图）+ `overview.png`（一次看全）+ **`raw/*.png`（未标注的输入图，可重跑）**；③ 要**改个阈值试一下**：`cargo run -p replay -- data/tasks/<任务ID> --min-confidence 0.70`。★ 图只能回答"看到了什么"，"为什么这么判"看右边的候选表 |
| **判断「是 OCR 读错了，还是判据判错了」**（同一张干净图上的争议） | `cargo run -p replay -- data/tasks/<任务ID> --ocr auto [--upscale 3]`：把 `raw/` 那张**未标注**的图喂回本机引擎，读数一比就有结论（`tools/replay/src/layer.rs` 的 `attribute`）。★ 只对**盘上没过**的步骤跑（过了的没有"卡在哪一层"可言）。★ 引擎的启动参数**没记进事件流**，所以默认不传参数；读数对不上时第一个该试的是 `--upscale`（同一张图、不同倍率的结论**可以不同**）。⚠️ 拿 `steps/*.png`（叠了框的）重跑是错的——那是另一套读数 |
| **把一次现场收编成一条回归用例**（T30 的「现场 → 用例」） | ① 现场材料在原位：`data/tasks/<任务ID>/`；② 收编时**整份拷成** `data/cases/<用例名>/`，本机回归跑它；③ 要进仓库的**只有人工造的假场景**：`tools/replay/tests/fixtures/`（人名为假，由 `tools/replay/src/tests.rs` 里那个 `#[ignore]` 生成器重跑重写）。★★ **现场与用例都只在 `data/` 下，永不入库**（`.gitignore` 的 `/data/`、`**/cases/`、`**/fixtures/**/*.png`）；代价是 CI 只能跑假场景，"OCR 级"的验证只有本机能做（见 `docs/todo.md` T30） |
| 改**事件流的格式**（加一种事件 / 加一个字段） | `src-tauri/src/task_diagnostics/events.rs`（**格式只有这一处定义**）+ `types.ts` 的 `ReplayEvent` + **`tools/replay`（读盘那侧要跟着改，否则离线重放读不懂）**。★ 加字段不算"含义变过"（`SCHEMA_VERSION` 不动），但要**同时**改 `tools/replay/src/tests.rs` 里造事件流的那两个助手，否则回归用的假场景会缺字段 |
| 改「过程重放」页显示哪一步 / 怎么配对 | `apps/desktop/src/replayView.ts`（配对规则，只此一处）+ `components/TaskReplay.tsx`（展示）。★ 同一张盘 `tools/replay/src/lib.rs::run` 也要一起看——两个消费者必须用同一条配对规则 |
| **改搜索式工作流**（工作流 3：点搜索框 → 下拉挑人 → 资料页 → 发消息） | `runner/search.rs`。资料页那一套（核验 / 滚到底 / 找入口）也在这里。★ 点下拉之后的**两种落点**都在 `open_chat_from_dropdown`：资料页没认到目标时**跳过**资料页两步、直接核验聊天标题（`docs/todo.md` T32） |
| **改列表扫描式**（原有那条：滚会话列表认名字） | `runner/list.rs` |
| **改「只做导航」**（工作流 1 / 2） | `runner/navigate.rs`（判据与点击）+ `vision/src/template.rs`（匹配算法） |
| **改「在哪一步停下 / 要不要发」** | `runner/message.rs` 的 `prepare_message`。**判据只有一处**：配置里的「只填不发」（`stop_before_send`）。勾了 ⇒ 停在 `Prepared`；没勾 ⇒ 逐字输入正文后**直接点发送按钮**（中间不插任何等人工的环节，2026-09-22 移除）。★ 与走哪条工作流**无关**——搜索式一度按工作流无条件停住，2026-09-22 已收掉（`docs/todo.md` T16） |
| **改「这条工作流到底要什么」**（要标哪些区域 / 要不要填联系人·正文） | **`src-tauri/src/runtime/requirements.rs`** 的 `required_marks` 与 `workflow_inputs`。★★ **判据只有这一处**：装配期按它拒绝、界面通过 `workflow_requirements` 命令读它渲染。★ 分叉的两种后果都很难查：`required_marks` 分叉 ⇒「界面说齐了、点开始却被拒」；`workflow_inputs` 分叉 ⇒「按钮点不动、也不说为什么」（2026-09-20 实测，见 T27） |
| **排查「某一步为什么慢 / 单步超时超在哪」** | ① 打开 `data/tasks/<任务ID>/task.log`：先搜 `超过单步上限`（文案里带**实际耗时**与**判据的代码位置**），再看它上面那一批 `⏱ …` 行——每条都带 `@文件:行`，走得最久的那条指的就是要开的地方；② 实测最常见的两段是 `导航·图标匹配 icons.locate`（`vision/src/template.rs`，逐位置扫搜索区 × 每个模板）与 `⏱ 单步图 NN`（`task_diagnostics.rs`：标注图的渲染/编码/写盘**跑在任务线程上**，直接吃单步预算）；③ 别先怀疑匹配阈值——阈值只决定"够不够格"，**不影响耗时**（见 `docs/todo.md` T31） |
| **给任务日志加一行** / 改开头那份配置快照 | `src-tauri/src/task_log.rs` 的 `write_start_header`。★ **别加回 `lib.rs`**（它贴着基线，那一百来行是整段搬出去的）。运行参数（模式 / 工作流 / 导航目标）一律取自 `choice`，与界面上选的必须一致 |
| **改「本次走哪条路」怎么传到后端**（**模式** / 工作流 / 导航目标） | `runtime.rs` 的 `RunChoice` + `lib.rs::start_task`（读 `request.run_choice`，**不读也不写配置**）+ `App.tsx::handleStart`（补进请求）+ **`components/RunChoiceFields.tsx`** 的三个选择器（绑 `runChoice`）。★★ **这是运行参数、不是配置项**——别改回读 `state.config`，那正是 2026-09-19「选了 A 跑的是 B」那个 bug。★ `nav_target` 的值域是**图标库目录名**（`String`），装配期在 `runtime.rs` 的 `build_runner` 里收敛成"要点这一个图标" |
| 改**模式**（演练 / 真实）影响到的行为 | ① 挑哪组端口 ② 真实模式没标定尺寸就拒 ③ 审计里的平台字段（`RuntimeMode::platform_label`）④ 要不要带标定窗口（`RuntimeMode::calibrated_window`）。★ 上面四条**全部**通过 `build_runner` 开头那句 `config.mode = choice.mode` 跟随本次运行参数，**判据只有一处**。界面侧：页头徽标 / `canSave` / 演练场景显隐一律看 `App` 的 `activeMode`，**别用 `draft.mode`**。提示文案只有一处：`RuntimeMode::notice()`，后端成对下发（`RuntimeInfo.mode_notices`）。★ 这几样**全在 `src-tauri/src/runtime/mode.rs`** 里，改模式相关的东西先开它 |
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
| 改 Win32 动作（点击/滚动/输入） | `platform-windows/src/desktop.rs`（契约实现）+ `winapi/input.rs`（键鼠/滚轮/文本原语）+ `winapi.rs`（窗口、截屏、剪贴板、进程） |
| **改光标怎么走 / 快慢 / 画圆** | 全在 `platform-windows/src/winapi/cursor.rs`：缓动在 `move_cursor`，圆周在 `circle_points`（按 `turns` 圈扫过）+ `move_cursor_circle`（`turns = 1 + CIRCLE_EXTRA_TURNS`，**终点不回起点**）。**纯计算部分另有用例**（`winapi/tests.rs`）—— 轨迹错了 `SendInput` 照样返回成功，只有用例拦得住。入口：命令 `draw_cursor_circle` → `api.ts::drawCursorCircle` → `CursorMotionPanel.tsx` |
| 改**截图的触发方式**（延时秒数、热键界面） | `components/CaptureTrigger.tsx`（界面）→ `src-tauri/src/capture_hotkey.rs`（命令）→ `platform-windows/src/hotkey.rs`（注册与消息循环）。⚠️ 延时秒数**刻意写死**，理由在该组件顶部 |
| 改**全局热键允许哪些键** | `platform-windows/src/hotkey.rs` 的 `HotkeyKey`。**判据只有这一处**：界面故意用自由文本输入而**不列候选表**，就是为了不出现第二处 |
| 排查「按了热键没反应」 | 先看界面上「已生效：Ctrl+Alt+S」在不在（不在 = 没注册成功，错误文案就在旁边）；再看 `capture_hotkey.rs` 的事件是否发到了界面 |
| 排查「鼠标不动 / 点了没反应」 | 先看 `task-<ID前8位>.log`（`REFERENCE.md` §11.2 的排查顺序）；★ 2026-09-21 起同一件事还有两样材料：`data/tasks/<任务ID>/`（逐步的**标注图** + `events.jsonl`）与界面的**「过程重放」**页（左图右表）。再动 `winapi/input.rs`（点击/滚轮）或 `winapi/cursor.rs`（光标怎么走） |
| **排查「窗口一闪就退出」/ 启动不起来** | 先看 `data/startup.log`（`startup_log.rs` 写的）。启动期故障**全发生在窗口出现之前**，这是唯一现场。★ 若连它都没有 ⇒ 程序根本没跑到 `run()`（缺 DLL、被拦、双击的不是这个 exe）。`run.cmd` 现在会**等 4 秒确认进程还活着**，失败就把这份日志打出来 |
| 改审计字段 / 加发送台账 | `storage/src/{db,store}.rs` + `automation-core/src/audit.rs` |
| 改证据脱敏方式 | `vision/src/evidence.rs`（涂灰逻辑）+ `storage/src/evidence.rs`（落盘与清理） |
| 加演练场景 / 故障注入 | `platform-mock/src/scenario.rs` + `fault.rs` |
| 加界面按钮（会真的产生输入） | `lib.rs` 加命令 + `api.ts` + `App.tsx`/对应组件。**确认它该不该按运行模式设限** |
| 改窗口识别规则（类名/exe） | `lib.rs` 的 `desktop_for` + `platform-windows/src/config.rs` |

## 5. 跨层改动清单（最容易漏一处的那种）

**加一个配置字段 —— 4 处**，漏任何一处都会「界面能填但不生效」或「保存后被静默丢弃」：

1. `apps/desktop/src-tauri/src/runtime.rs` → `RuntimeConfig` 加字段（带 `#[serde(default)]`）
2. `apps/desktop/src/types.ts` → 同名字段（**注意 serde 的命名转换**）
3. `apps/desktop/src/components/` → 表单控件。**放哪个文件看归属**：
   要"保存配置"的 → `RuntimePanel.tsx` / `TargetWindowSection.tsx` /
   `AdvancedParamsSection.tsx` / `TypingTextSection.tsx`；
   只管"这一次怎么跑"的 → `RunChoiceFields.tsx`（绑 `runChoice`）
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
   **不许**在界面里另列一张表（渲染它在 `RunChoiceFields.tsx`）
6. `apps/desktop/src/types.ts` 的 `Workflow` 联合类型 + `RunChoiceFields.tsx` 的
   `WORKFLOWS` / `WORKFLOW_LABELS` 下拉项

⚠️ 第 5 步漏了的表现是「界面说齐了、点开始却被拒」——装配期会用 `required_marks`
拒绝，而人只会去怀疑标定本身。`ipc_flow.rs::the_requirement_list_matches_what_assembly_enforces`
就是拦这个的。

⚠️ **运行参数那条线不用动**：新工作流是随 `StartTaskRequest.run_choice` 走的
（`RunChoice.workflow` 是个枚举，加一项自动就通了）。要动的是上面第 6 步的下拉项——
它绑的是 `runChoice`，不是配置草稿（见 `docs/windows-mvp-interface.md`）。

**加一个 IPC 命令 —— 3 处**：`lib.rs` 写 `#[tauri::command]` → `with_commands` 里注册
（`lib.rs:1846` 的 `generate_handler!`，**漏了会在运行时才报「命令不存在」**）→ `api.ts` 加封装。
⚠️ **注册点是唯一的一处**：命令写在别的模块里（`capture_hotkey.rs` / `cursor_trace.rs`）
也照样注册在这里，**别在那边另挂一个 `invoke_handler`**。跨模块的命令必须 `pub`，
否则报 `E0603: macro import … is private`（错指向 `generate_handler!`，看着像宏的问题）。
命令带 `AppHandle<R>` 时，**顺手补一条"能调通"的用例**（`ipc_flow.rs` 里已有五条先例）：
漏注册的表现是前端拿到 "command not found"，而**编译期毫无提示**。
⚠️ 用例会**真的产生副作用**时（动鼠标、占热键），加 `#[ignore]` 并在文档注释里写清
"为什么默认不跑 + 怎么手动跑" —— 一个会劫持操作者鼠标的用例比没有用例更糟。

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
| `tools/replay/src/{tests,ocr_tests}.rs` | 360 / 295 | **离线重放**（判据重跑 + 报告排版）与**真 OCR 归因**（假引擎，不依赖现场素材；`#[cfg(test, unix)]`） | `cargo test -p replay` |

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
