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
「现在」是 2026-09-19 三条工作流那批改动之后的实测值：

| 文件 | 基线 | 现在 | |
| --- | --- | --- | --- |
| `apps/desktop/src-tauri/src/lib.rs` | 1903 | 1847 | ✅ 减 |
| `crates/automation-core/src/runner/mod.rs` | 1395 | 1224 | ✅ 拆成 5 个文件 |
| `apps/desktop/src-tauri/src/runtime.rs` | 1039 | 893 | ✅ 减 |
| `crates/vision/src/template.rs` | 881 | 667 | ✅ 减 |
| `crates/platform-windows/src/winapi.rs` | 862 | 1094 | ⚠️ +232（其中 +152 是光标轨迹）|
| `apps/desktop/src/components/IconLibraryPanel.tsx` | 983 | 1064 | ⚠️ +81 |
| `apps/desktop/src/components/RuntimePanel.tsx` | 596 | 782 | ⚠️ +186 |

**已移出**：`apps/desktop/src-tauri/src/icon_library.rs`（1030 → 195）
与 `calibration.rs`（604 → 370），都拆成了「薄接口 + 子模块目录」：
`icon_library/{layout,read,write}.rs`、`calibration/{catalog,tests}.rs`。
`icon_library/tests.rs`(470) / `calibration/tests.rs`(287) 是测试，不受行数限制。

**2026-09-19 又移出三处**，都是同一条思路——**先看测试模块占了多少**：

| 原文件 | 现在 | 抽出的测试 |
| --- | --- | --- |
| `automation-core/src/runner.rs`(1395→2136) | `runner/mod.rs` 1224 + 4 个子模块 | 见下 |
| `apps/desktop/src-tauri/src/runtime.rs`(1541) | 893 | `runtime/tests.rs` 653 |
| `crates/vision/src/template.rs`(1059) | 667 | `template/tests.rs` 396 |
| `crates/automation-core/src/state.rs`(532) | 261 | `state/tests.rs` 275 |
| `apps/desktop/src-tauri/src/lib.rs`(1931) | 1847 | `src/tests.rs` 88 |

`runner/` 那四个子模块是**按工作流**分的，不是按"随便切几刀"：
`search.rs`(440) 搜索式、`list.rs`(261) 列表扫描式、`navigate.rs`(163) 导航图标、
`message.rs`(131) 填正文。理由写在各文件头的模块文档里。
⚠️ 子模块里的 `impl Run<'_> { … }` 方法一律是 `pub(super) fn`——**必须加**，
私有意味着"只有定义它的模块及其子孙可见"，父模块的 `execute()` 就调不到了。

⚠️ `CalibrationPanel.tsx` 一度涨到 529 行（加失效项清理时），已拆出
`CalibrationSteps.tsx`(132) 与 `StaleMarksNotice.tsx`(48)——那两处拆分确实在，
但**主文件后来又长回去了**：2026-09-19 实测 **722** 行，本节原先写的「回到 462」是错的。
同理 `types.ts`（实测 676）**从来没进过上面那张表**。这两处的实测值记在
`docs/todo.md` T17（不往上面那张表里加：§9 明说那张表是存量）。
**拆之前先看能不能拆**，别直接把新超标的文件加进上面这张表——
这张表是**存量**，往里加等于把规矩作废。

⚠️ **还欠着的**（写在 `docs/todo.md` 的 T17）：`RuntimePanel.tsx`(+186)、
`IconLibraryPanel.tsx`(+81)、`winapi.rs`(+53) 这三处这轮**只涨没拆**。
它们不是测试撑起来的，得真做拆分设计（提组件 / 拆原语），
不像抽测试那样机械。**别以为它们已经被处理过了。**

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
