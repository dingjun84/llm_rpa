# 编码规范

> 适用：本仓库全部源码（Rust / TypeScript / TSX），含测试。
>
> **与 `docs/architecture.md` 冲突时，以架构文档为准**——那里写「为什么」，
> 这里只写「怎么写」。本文不复述设计理由，需要理由时去看架构文档与
> `.workbuddy-ai/memory/REFERENCE.md`（实测结论）。
>
> 规范的目的不是让代码好看，是让**改动可预期**：新会话、半年后的自己，
> 都能在同样的位置找到同样的东西。

## 1. 三条底线（其余都可以商量，这三条不行）

1. **逻辑正确优先于一切形式要求。** 为满足本规范而做的改动必须**保持行为不变**；
   拆分是纯结构重构，拆完必须跑测试证明它没改行为。
2. **分层不许破。** 业务流只依赖 `automation-core` 的 trait，
   不直接调 Win32 / OCR SDK / 输入库。
3. **判据只有一处。** 同一个判断（文字是否等于目标名、分数够不够、画面变没变）
   只允许有一个权威实现。看到第二处在写 `==` 比目标值，
   先问「这是不是别处已经定义过的判据」。

## 2. 规模上限

| 对象 | 目标 | 上限 | 超了怎么办 |
| --- | --- | --- | --- |
| 源码文件（`.rs` / `.ts` / `.tsx`） | 300 行 | **500 行** | 按职责拆文件（§3） |
| 函数 / 方法 | 40 行 | **80 行** | 按阶段拆方法（§3） |
| 单个 `#[test]` | 30 行 | **60 行** | 抽公共 setup，别复制粘贴 |
| 测试文件 | 不限 | 不限 | 按场景组织，别按行数切 |
| 嵌套深度 | ≤ 3 | **4** | 提前 `return` / 用 `?` 消掉一层 |
| 函数参数 | ≤ 4 | **5** | 打包成结构体 |

**计数口径**：文件行数含注释与空行（`wc -l` 的口径，便于机械检查）；
函数行数从 `fn` 签名到闭合花括号，**不含**其上方的文档注释。

超出上限的两条路，按顺序走：

1. **先拆**——按职责 / 阶段拆，不是按行数对半切。
2. **确实拆不动**，就地写明**为什么**拆不动、**代价**是什么。
   这与「不要写死」是同一条规矩：允许例外，但例外必须留痕。

> 拆不动的典型情形：Win32 调用序列里每一步都依赖上一步的中间状态，
> 硬拆会把中间状态提升成参数，反而更难读。`winapi.rs` 里存在这种情况。

## 3. 拆分与复用

### 怎么拆

- **按阶段拆，不按行数拆。** `runner.rs::execute()` 271 行，它的问题不是「太长」，
  而是「一屏里挤了十个阶段」。该拆出来的是 `navigate_to_view()` /
  `locate_contact()` / `sweep_contact_list()` 这种粒度的东西——已经部分这么做了，继续。
- **优先拆成 `&mut self` 上的私有方法**，而不是自由函数。
  自由函数会逼你把字段一个个传进去，拆完参数从 3 个变成 10 个，等于没拆。
- **判据跟着判据走。** 拆分时若动了某个判断，确认它仍然只有一处权威实现（§1.3）。
  把判据复制进新函数 = 制造一个未来必然不一致的地方。
- **拆完立刻跑测试。** 拆分不改变行为，测试是唯一能证明这一点的东西。

### 复用

- **三次法则**：同样的逻辑出现**三次**再抽象。两次相似就抽象，
  往往造出一个两边都不合身的东西。
- **跨 crate 复用走 trait**，走 `automation-core` 的端口。
  不许 `vision` 直接调 `storage` 的函数——那会造出反向依赖（§6）。
- 复用**不等于**共用可变状态。宁可传参，不要为了「复用」引入全局单例。

## 4. 精简的边界

**要精简的**：

- 注释掉的代码、死代码、`dbg!` / `console.log`、失效的 `#[allow]`；
- **投机性抽象**：只有一个取值的开关、用不上的泛型参数、
  「以后可能要用」的配置项——用不上就删，真要用时再加；
- 重复的 `match` 分支、能 `?` 掉的 `if let Ok(x) = ...` 嵌套。

**不要精简的**：

- **注释里解释「为什么」的部分。** 本仓库的注释是资产：
  「为什么这个值不能是 0」「为什么容差是 1 而不是 0」「为什么这个失败不转人工」——
  这些不是冗余，是防止后人凭直觉改坏它的唯一屏障。**精简的对象是代码，不是理由。**
- 错误信息里的**具体原因与排查方向**。`Failed("内部错误")` 不是精简，是丢信息。

## 5. 错误处理

- 生产代码里，**有业务含义的失败禁止用 `.unwrap()` / `.expect()` 吞掉**，
  一律映射成 `AutomationError`，并如实回答 `is_retryable()` 与 `needs_human_review()`。
- **不许为了让流程继续而谎报这两个判据。** 把「不确定」报成「可重试」，
  等于把「不确定即失败」这条产品底线拆了。
- **不重试到成功。** 重试只走 `runner.rs` 的 `with_retry()`，
  且只有平台层 I/O 与超时算可重试。
- 允许 `unwrap()` 的地方：测试代码；以及失败即不可恢复的程序性错误（如锁中毒），
  但要就地写明。

> 存量：`src/` 下约 190 处 `unwrap` / `expect`（含 14 个文件的 `#[cfg(test)]` 模块）。
> **不强制整改**，新增代码按本节执行。

## 6. 依赖与分层

| 规矩 | 检查方式 |
| --- | --- |
| `automation-core` 保持**零内部依赖** | `crates/automation-core/Cargo.toml` 只应有 thiserror / uuid / serde / sha2 |
| 实现 crate 之间**互不依赖**，都只指向 core | `vision` 不许依赖 `storage` |
| 加新依赖前先问：标准库或现有依赖能不能做 | 能就不加 |
| 第三方版本统一写 `[workspace.dependencies]` | 子 crate 用 `.workspace = true` |

`platform-windows` 在 src-tauri 里挂在 `[target.'cfg(windows)'.dependencies]`，
**不是**普通 `[dependencies]`——在 dependencies 里找不到别以为没引用。

## 7. `unsafe` 的边界

- `unsafe` **只允许出现在 `platform-windows`**（当前 50 处，全是 Win32 FFI）。
  其他 crate 出现 `unsafe`，说明分层被破坏了。
- 每个 `unsafe` 块必须紧跟 `// SAFETY:` 注释，写明**为什么在这里是安全的**
  （指针从哪来、生命周期由谁保证、失败会怎样）。
  没有 SAFETY 注释的 `unsafe` 视为未完成。

## 8. 命名与注释

- 注释、文档、错误文案一律**中文**。
- 布尔字段用**肯定式**：`liveness_check`，不要 `disable_liveness_check`。
  否定式开关在传参和取反时必然出错。
- 名字与 `types.ts` / 配置字段**保持一致**（serde 命名转换见
  `docs/windows-mvp-interface.md`）。前后端不一致是排查成本最高的一类问题。
- 公开的类型与函数写文档注释。**解释「为什么」的注释不嫌长，
  解释「是什么」的注释不写**——代码自己会说。
- **用户可见的文案里不写 Markdown 标记**。`**加粗**` 在界面上渲染不出来——
  前端**没有** Markdown 渲染器，它只会原样显示两个星号（`docs/code-map.md` §3.5 的
  `hint` 是直接 `{item.hint}` 渲染的）。按文案的来路分两种写法：
  - **跨进程传的字符串**（Rust 的 `hint`、错误文案、`println!` 的输出）一律**纯文本**。
    要强调就靠句子结构。这些字符串的渲染点太多太散，为它加一个渲染器等于多一个机制。
  - **JSX 文本**里要强调写 `<strong>`（项目里一直是这么写的，见
    `CalibrationPanel.tsx` / `IconLibraryPanel.tsx`）。
  - ⚠️ **注释**（含 `///` 文档注释）里可以照常用 `**`，那些是读源码用的，不进界面。
  自查：`grep -rn '\*\*' apps/desktop/src --include=*.tsx` 会把注释一起报出来，
  **只看那些不在注释里的行**；Rust 侧同理（要区分「在字符串里」还是「在注释里」）。

## 9. 存量超标清单（基线，2026-09-19 实测）

**不要求立即重构。** 规矩是**只减不增**：改动这些文件时顺手能拆就拆，
不要在里面再添新职责。

超过 500 行的生产文件（7 个）。「基线」是这条规矩的**参照值**，
「现在」是 2026-09-19 **当天第四批改动**（T23 收尾：模式也改成运行参数）之后的实测值：

| 文件 | 基线 | 现在 | |
| --- | --- | --- | --- |
| `apps/desktop/src-tauri/src/lib.rs` | 1903 | 1793 | ✅ **减了 110**。T27 把**开跑前那份配置快照**（一百来行，纯写日志）整段搬成 `task_log.rs`(159) —— 它本来就是一段"写日志"的活，与命令层"装配 / 登记 / 起线程"不是一回事。见 `docs/todo.md` T27 |
| `crates/automation-core/src/runner/mod.rs` | 1395 | 1224 | ✅ 拆成 5 个文件 |
| `apps/desktop/src-tauri/src/runtime.rs` | 1039 | 927 | ✅ **减了 85**。T27 把「这条工作流要什么」那一组（`required_marks` / `missing_marks` / `WorkflowRequirement` / `workflow_inputs`）整段搬成 `runtime/requirements.rs`(171)，父模块 `pub use` 再导出（同 `mode.rs` 的做法）。见 `docs/todo.md` T17 / T27 |
| `crates/vision/src/template.rs` | 881 | 667 | ✅ 减 |
| `crates/platform-windows/src/winapi.rs` | 862 | 796 | ✅ **已拆完**（1215 → 985 → **796**）。两刀：光标轨迹 → `winapi/cursor.rs`(267)、输入原语 → `winapi/input.rs`(228)。见 `docs/todo.md` T17 |
| `apps/desktop/src/components/IconLibraryPanel.tsx` | 983 | 800 | ✅ **已拆**（1064 → 808 → 800）。列表搬成 `IconList.tsx`(204)，两段结果预览搬成 `NavResultSections.tsx`(140)。T26 删掉「用于聊天历史导航」那一组后又减了 8 行 |
| `apps/desktop/src/components/RuntimePanel.tsx` | 596 | 288 | ✅ **已拆**（858 → 288）。按「归属」拆成四个同级组件：`RunChoiceFields`(270，运行参数) / `TargetWindowSection`(202) / `AdvancedParamsSection`(192) / `TypingTextSection`(87)。见 `docs/todo.md` T17 |

⚠️ **`lib.rs` 这次给出了一个可复用的做法**：新功能有近百行、眼看着要破基线时，
**先把它整段搬成一个同级模块**（`cursor_trace.rs`，与 `capture_hotkey.rs` 同款），
而不是硬往里塞。搬移的要点：
① 命令**仍然只在 `lib.rs` 的 `with_commands` 里注册一处**；
② 跨模块的命令必须是 `pub`，否则 `E0603: macro import … is private`
（错指向 `generate_handler!`，看着像宏的问题）；
③ 模块自己 `use` 它要的（`Manager` 忘了加的表现是 `E0599: no method named
get_webview_window`）。**搬完 `wc -l` 对一眼**，别信脚本的自报数。
⚠️ **`RuntimePanel.tsx` 已经拆完**（2026-09-19）：858 → **288**，见 `docs/todo.md` T17。
★ 拆 `.tsx` **不要用搬运脚本**（`large-file-mechanical-split` skill 里也写了这条）：
提组件要先设计 props，脚本只能搬、搬完还得改 import，手改更快。
这次用的判据是**按"归属"分组**，一句话：**凡是要"保存配置"的留在主文件，
只管"这一次怎么跑"的搬走**。这一刀之后主文件只剩页头提示 + 保存按钮 + 底部说明。

**已移出**：`apps/desktop/src-tauri/src/icon_library.rs`（1030 → 195）
与 `calibration.rs`（604 → 370），都拆成了「薄接口 + 子模块目录」：
`icon_library/{layout,read,write}.rs`、`calibration/{catalog,tests}.rs`。
`icon_library/tests.rs`(470) / `calibration/tests.rs`(287) 是测试，不受行数限制。

**2026-09-19 又移出三处**，都是同一条思路——**先看测试模块占了多少**：

| 原文件 | 现在 | 抽出的测试 |
| --- | --- | --- |
| `automation-core/src/runner.rs`(1395→2136) | `runner/mod.rs` 1224 + 4 个子模块 | 见下 |
| `apps/desktop/src-tauri/src/runtime.rs`(1541) | 959 | `runtime/tests.rs` 711 |
| `crates/vision/src/template.rs`(1059) | 667 | `template/tests.rs` 396 |
| `crates/automation-core/src/state.rs`(532) | 261 | `state/tests.rs` 275 |
| `apps/desktop/src-tauri/src/lib.rs`(1931) | 1894 | `src/tests.rs` 88 |
| `apps/desktop/src-tauri/src/lib.rs`(1993) | 1896 | `src/cursor_trace.rs` 141（**不是测试**，是整段功能搬出去；见上面那条 ⚠️）|

★ **同一条思路再来一次**（2026-09-19 第四批）：启动期落日志（`src/startup_log.rs`，165 行）
也是**整段新功能、不是测试**。⚠️ 但它没能把 `lib.rs` 压下去——同轮又加了 `ModeNotices`
与"任务日志改记请求里的模式"那几行，净涨 19（1896 → 1922，**破了基线**）。

★★ **然后又把 `ModeNotices` 搬进 `runtime/mode.rs`**，`lib.rs` 1922 → **1900**（✅ 回到基线以下）。
**教训值得记**：搬出去一大块（`cursor_trace.rs`）之后，**新功能会继续往 `lib.rs` 里填**，
一两轮就又回去了。所以往 `lib.rs` 加东西之前，**先问一句"这块能不能跟着某个已有的
子模块走"**——`ModeNotices` 就是这么找到家的：它讲的正是"两种模式各自是什么"，
跟 `RuntimeMode::notice()` 是同一件事，搬过去不用改任何设计。
★ `lib.rs` 若还要减，下一个切口是 `start_task`（225 行）：
把"登记任务 + 落日志快照"整段提成 `task_registry.rs`。那是核心命令，动它要单独一轮。

`runner/` 那四个子模块是**按工作流**分的，不是按"随便切几刀"：
`search.rs`(440) 搜索式、`list.rs`(261) 列表扫描式、`navigate.rs`(163) 导航图标、
`message.rs`(131) 填正文。理由写在各文件头的模块文档里。
⚠️ 子模块里的 `impl Run<'_> { … }` 方法一律是 `pub(super) fn`——**必须加**，
私有意味着"只有定义它的模块及其子孙可见"，父模块的 `execute()` 就调不到了。

⚠️ `CalibrationPanel.tsx` 一度涨到 529 行（加失效项清理时），已拆出
`CalibrationSteps.tsx`(132) 与 `StaleMarksNotice.tsx`(48)——那两处拆分确实在，
但**主文件后来又长回去了**：2026-09-19 实测 **722** 行，本节原先写的「回到 462」是错的。
同理 `types.ts`（实测 710）**从来没进过上面那张表**。这两处的实测值记在
`docs/todo.md` T17（不往上面那张表里加：§9 明说那张表是存量）。
**拆之前先看能不能拆**，别直接把新超标的文件加进上面这张表——
这张表是**存量**，往里加等于把规矩作废。

⚠️ **还欠着的**（写在 `docs/todo.md` 的 T17）：`CalibrationPanel.tsx`(+260)、
`types.ts`(710，没记过基线)。这两处不是测试撑起来的，得真做拆分设计
（提组件 / 拆原语），不像抽测试那样机械。**别以为它们已经被处理过了。**
★ `winapi.rs` 最大的一块（光标轨迹）**已经拆完**（2026-09-19）：见下面 §9 末尾
「第二次照抄」那一节。剩下的三批各自也能成文件，但**别再照抄一次就完事** ——
先把「谁依赖谁」列清楚（`mouse_move_input` 这类**被搬走那段用的私有原语**留在
父模块就行，子模块用 `super::` 拿得到）。

★★ **`runtime.rs` 那一处已经拆完了**（2026-09-19，同一天里破基线又压回来）：
`RuntimeMode`（含 `platform_label` / `calibrated_window` / `notice`）与 `DemoScenario`
搬成了 `runtime/mode.rs`(127)，`runtime.rs` 1049 → **984**。
**这一刀是可以照抄的做法**，下次再有"破基线"的文件可以照这个顺序来：

1. 先找**自成一体的那一块**（这里 = 纯数据 + 纯函数，不碰端口/配置其余部分/装配流程）；
2. 建**同级子模块**（`runtime/mode.rs`，与 `runtime/tests.rs` 平级）；
3. ★ 逐个调可见性 —— `pub(super)` 给"只被父模块调用"的，`pub` 给"外面也在用"的
   （这里 `notice()` 被 `lib.rs` 调 ⇒ `pub`；另两个只被 `runtime.rs` 调 ⇒ `pub(super)`）。
   **漏了这一步的症状是 `E0624: method is private`，指向调用点而不是定义处**；
4. ★ **父模块加 `pub use`** 保住原来的公开路径
   （`pub use mode::{DemoScenario, RuntimeMode};`）。少了它是一堆
   `E0432: unresolved import`，看着像"文件没编进去"；
5. 收尾：`cargo check --tests` 会顺手报出**因为搬走而变成未使用的 import**
   （这次是 `CalibratedWindow`），删掉它。

★★ **第二次照抄：`winapi.rs` 的光标轨迹段**（2026-09-19，同一套顺序，1215 → 985）。
同款五步，但有**三处只有"跨文件依赖"才会出现**的东西，下次照抄时按顺序检查：

1. ★ **搬走的那段用到的私有原语，留在父模块就好**。`mouse_move_input` 是父模块的
   **私有 `fn`**，但它**只**被搬走的那段调用 —— 不用跟着搬：**子模块能访问祖先的私有项**
   （隐私是「定义处 + 其后代」），子模块里 `use super::mouse_move_input;` 就够。
   判据：**别因为"它只被这块用"就搬**，搬了反而要反过来给父模块开可见性。
2. ★★ **被搬走的 import 要显式搬，不能只靠 `use super::*`**。
   `SM_XVIRTUALSCREEN` 等四个常量原本只在父模块 `use`，搬走后在父模块变成
   **unused import**（`use super::*` **不算**"用掉了父模块的 import"）。
   做法：子模块**自己写全 import**（`Duration` / `SendInput` / `INPUT` /
   四个 `SM_*VIRTUALSCREEN` / `super::{cursor_position, mouse_move_input, WinResult}`），
   父模块把那四个常量从 `use` 里删掉。父模块的 `//!` 文档里
   「unsafe 都集中在本模块」也要补一句（含子模块）。
3. ★ **`pub use` 不能给"原本私有"的常量用**。`CIRCLE_MIN_STEPS` 原来是私有
   `const`，同级测试 `winapi/tests.rs` 靠 `super::` 取它。若把它 `pub use` 出去
   会**放宽公开面**（父模块 `winapi` 是 `pub`）⇒ 改成 `pub(super) const` +
   **改测试的 import 路径**为 `use super::cursor::{...}`（`tests` 是 `winapi` 的
   后代，够得着私有的 `mod cursor`）。只有真正对外的那三个
   （`move_cursor` / `move_cursor_circle` / `CircleTrace`）才 `pub use`。

★ **搬运脚本的校验要允许"刻意的差异"**。这次的多重集比较报出 4 missing / 31 extra，
其中 3 处是**我自己有意改的**（文档行、import 两行、`CIRCLE_MIN_STEPS` 那行），
其余是模块文档 / `use` 行 / 空行。**校验脚本第一次就把它们当"意外"拦下来了** ——
这正是它该做的：**先把差异全列出来，逐条确认是"有意的"再放行**，
不要为了让它通过就把 `extra` 一律放过。

超过 80 行的函数（主要几处，`~` 为估算）：

`runner/mod.rs::execute`(~271)、`lib.rs::start_task`(225)、`lib.rs::click_icon`(~206)、
`lib.rs::probe_nav_icon`(~135)、`runtime.rs::is_valid`(~147)、
`tools/winocr/src/main.rs::run`(~136)、`runner/navigate.rs::navigate_to_view`(~105)。

已有的两处 `#[allow(clippy::too_many_arguments)]`（`lib.rs` 的
`save_icon_from_crop` / `click_icon`）——优先改成参数结构体，而不是继续加 `allow`。

### 抽测试模块这件事本身有个坑

搬 `#[cfg(test)] mod tests { … }` 到同级 `tests.rs` 时，要对内容整体左移 4 格。
两个地方会咬人，**都不会报错**：

1. **空行**不能用 `""` 占位。`"".join(lines)` 里空串一个换行都不贡献，
   结果整段代码的空行**全部消失**。判据：`wc -l` 与脚本自报的行数对不上。
2. **`lines` 里每个元素只有一行**（`splitlines(keepends=True)` 的产物）。
   拿 `"#[cfg(test)]\nmod tests {"` 这种含换行的串去比单行，永远不成立，
   报出来的错是「找不到模块」，看着像文件格式不对。


## 10. 检查

```bash
# 超长生产文件（不含 tests/ 与 examples/；`*tests.rs` 是测试文件，一并排除）
find crates apps/desktop/src apps/desktop/src-tauri/src tools \
  \( -name "*.rs" -o -name "*.ts" -o -name "*.tsx" \) \
  | grep -v "/tests/\|/examples/\|/tests\.rs$" | xargs wc -l | awk '$1>500 && $2!="total"'

# 全量测试（--no-fail-fast 不能省，否则首个失败目标会中断整轮）
CARGO_INCREMENTAL=0 cargo test --workspace --no-fail-fast

# 静态检查
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets
```

> 本仓库目前**没有 CI 门禁**，以上命令靠自觉执行。
> 若日后要加门禁，从「超长文件」和 `cargo clippy` 这两条开始，
> 别一上来就卡函数行数——那会把现有的正常改动全堵死。
