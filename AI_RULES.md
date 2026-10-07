# 给 AI 的强制指令

> **这份文件是给 AI 助手读的,不是给人读的。**
> 它的存在理由:本仓库的 `docs/` 写得极好(code-map 145KB、CONVENTIONS 22KB、
> todo 144KB),但那些是**给人读的散文**。AI 每次新会话不会自动去读它们 ——
> 结果是「改判据时忘了跑 replay」「新增 IPC 命令忘了注册」这类事情反复发生。
>
> 这里把**散落在 docs 里的强制项**提炼成可执行的指令。
> 规矩的完整解释一律看 `CONVENTIONS.md`;「改 X 开哪个文件」一律看 `docs/code-map.md`。

---

## 0. 每次动手之前(不可跳过)

接到任何改动请求后,先做这两件事,再写第一行代码:

1. **读 `docs/code-map.md` 的第 4 节「需求 → 文件(反向索引)」**,定位到 1~3 个文件。
   不要一上来就 Glob/Grep 全库 —— 仓库 4.6 万行、48 个源文件,其中 5 个占了四成代码量,
   而 90% 的改动只落在那 5 个里。
2. **读 `CONVENTIONS.md`**(规模上限、拆分与复用、错误处理、`unsafe` 边界)。

如果你要改的文件在生产文件基线表(`baselines.toml`)里,先确认它当前行数 ——
**只许减,不许增**。

---

## 1. 判据只有一处(本项目最重要的约定)

这个仓库反复强调的一件事:**同一个判断,代码里只许有一处实现。**

违反它的表现是「A 处放宽了,B 处没跟上」,而两个地方单独看都是对的,极难排查。
`docs/todo.md` 里 T7 就是一次真实的教训。

具体到常见改动:

| 你在改什么 | 判据本体在哪(只许改这一处) |
|---|---|
| 联系人姓名匹配 | `crates/automation-core/src/policy/trail.rs` |
| 搜索下拉挑人 | `crates/automation-core/src/dropdown.rs::judge_dropdown` |
| 停不停 / 发不发 | `automation-core/src/runner/message.rs` 的 `stop_before_send` |
| 这条工作流要什么 | `apps/desktop/src-tauri/src/runtime/requirements.rs` |
| 事件流格式 | `apps/desktop/src-tauri/src/task_diagnostics/events.rs` |
| 重放配对规则 | 前端 `replayView.ts` **与** `tools/replay/src/lib.rs::run`(两处必须同步) |
| 数据目录在哪 | `src-tauri/src/data_dir.rs` |
| 标定框校验 | `automation-core/src/regions.rs::RelativeRegion::validate` |
| 热键允许哪些键 | `platform-windows/src/hotkey.rs::HotkeyKey` |

**改完判据,必须跑离线重放看旧现场结论变没变:**

```bash
cargo run -p replay -- data/tasks/<任务ID> --min-confidence 0.70
```

---

## 2. 跨层改动清单(漏一处 = 静默失效)

这些清单在 `code-map.md` §5 有完整版。**漏掉的后果全是「编译通过但行为错」**:

- **加一个配置字段 → 4 处**:`runtime.rs` 的 `RuntimeConfig` → `types.ts` → 表单组件
  (按归属放进 `RuntimePanel` / `TargetWindowSection` / `AdvancedParamsSection` /
  `TypingTextSection`,或只管本次运行的 `RunChoiceFields`) → `build_runner` 映射。
- **加一条工作流 → 6 处**(其中第 5 处 `required_marks` 漏了的表现是
  「界面说齐了、点开始却被拒」)。
- **加一个 IPC 命令 → 3 处**:`#[tauri::command]` → **`with_commands` 里注册**
  (漏了**运行期**才报 "command not found",编译期零提示)→ `api.ts` 封装。
  **跨模块的命令必须 `pub`**,否则报 `E0603`(错指向 `generate_handler!`)。
- **加一个任务状态 → 4 处**;加一个区域 → 3 处;加一个端口方法 → 4 处
  (最易漏的是「替身没跟上」,且替身要能表达真实实现的**所有失败形态**)。

---

## 3. 改完必须跑的验证(按改动类型)

```bash
# ① 先激活工具链（多机位置不同，见 §9.1）。这一步不能省，
#    否则 cargo 找不到，你会以为"跑了"其实一条都没跑。
source scripts/rust-env.sh

# ② 规模 + 架构门禁（秒级，必跑）
python checks/size_gate.py
python checks/arch_gate.py

# ③ 按改动类型跑（--no-fail-fast 不能省，否则首个失败目标会中断整轮）
cargo test --no-fail-fast -p platform-mock    # 改了状态机/流程 → 必跑端到端
cargo test --no-fail-fast -p replay           # 改了判据 → 必跑离线重放
cargo test --no-fail-fast -p vision           # 改了匹配算法
cargo test --no-fail-fast -p automation-core  # 改了核心判据
cargo test --no-fail-fast -p storage          # 改了落库/审计

# ④ 静态检查（只跑工具链可用的 crate，原因见 §9.1）
CARGO_INCREMENTAL=0 cargo clippy --all-targets \
  -p automation-core -p platform-mock -p storage -p vision \
  -p replay -p sigma-drift -p winocr
```

⚠️ **`-p desktop` 在本机跑不了**（Tauri 依赖链里的 `schemars 0.8.22` 编译失败，
见 §9.1），CI 的 Windows job 上能跑。改 IPC 层时的替代办法：至少跑
`cargo check -p desktop --lib` 看能否编译，跑不通就如实说明没验证。

**关于测试文件**:`mvp_flow.rs`(2675 行)与 `ipc_flow.rs`(1810 行)是回归防线的主体。
`ipc_flow.rs::the_requirement_list_matches_what_assembly_enforces` 专门拦
「界面与装配期判据分叉」—— 改 `requirements.rs` 时它会告诉你有没有漏。

**会真的动鼠标 / 占系统热键的用例**一律 `#[ignore]`,并在文件头写清怎么手动跑。
一个会劫持操作者鼠标的用例比没有用例更糟。

---

## 4. 债务必须锚在代码现场

`docs/todo.md` 有 32 条 T 编号,写得极细。但**代码里 0 个 TODO 标记** ——
这意味着 AI 改到某段代码时,不知道这段代码有已知债务(它不会去翻 144KB 的 todo.md)。

**因此:每当你改动的代码正对应某条 T(比如 T9 / T15 / T17 / T31),在该处加一行锚点:**

```rust
// TODO(T9): 模板匹配目前是纯 Rust 实现,不是 OpenCV。见 docs/todo.md T9。
```

**反过来也成立**:新增一条 T 时,如果它对应当前代码里的某个位置,回去加锚点。
让债务**双向可跳转**:从代码能查到文档,从文档能定位到代码。

---

## 5. 规模纪律

- 生产文件 **≤500 行**;超了就必须拆,做法照抄 `CONVENTIONS.md` §9 的
  「先搬出一段自成一类的功能」(`lib.rs`、`runtime.rs`、`winapi.rs` 都是这么拆的)。
- **拆完顺手把 `baselines.toml` 里的数字改小** —— 只许减,不许增。
- 新增文件**一律受 500 行约束,不许进基线表**(不许给新代码开口子)。
- `#[allow(...)]` 优先改成结构性修法(如参数收成结构体),而不是继续加豁免。
  现有 3 处豁免在 `lib.rs:1814`、`lib.rs:1914`、`dropdown.rs:352`。
- 不留死代码、注释掉的代码、`dbg!`、`console.log`。
  ⚠️ 已知一处 dead code:`vision/src/template.rs` 的七档金字塔(`TEMPLATE_SCALE_PYRAMID`),
  它不在主路径上(`locate` 是单尺度),别照它去算耗时。

---

## 6. 分层与依赖方向(现在是机器检查的)

`code-map.md` §2 那张图不是参考,是**约束**:

- `automation-core` **零内部依赖**。往它加依赖 = 破坏分层,先停下来想清楚。
- 四个实现 crate(`platform-windows` / `platform-macos` / `platform-mock` / `vision` /
  `storage`)**互不依赖**,都只指向 core。
- `platform-windows` 的 `vision` 是 **dev-dependency**,**只许 `examples/` 与 `tests/` 用**。
  生产代码一旦 `use vision::` 就报违规 —— 这条由 `checks/arch_gate.py` 拦。
- 前端 `apps/desktop/src` 不直接导入 `src-tauri` 的类型(走 `types.ts` 的镜像定义)。

`python checks/arch_gate.py` 会校验以上全部。**新增内部 crate 时,
在 `checks/arch_gate.py` 的 `ALLOWED_INTERNAL_DEPS` 里登记它**。

---

## 7. 文档同步(改完顺手,不要攒)

| 你改了什么 | 必须同步 |
|---|---|
| 文件职责 / 行数 / 关键符号 | `docs/code-map.md` 对应那一行 |
| 架构决定 / 否掉的方案 | `docs/architecture.md` |
| 新增债务 / 关闭债务 | `docs/todo.md`(带 T 编号) |
| 基线数字 | `baselines.toml` |

**地图腐化比没有地图更坏** —— 它会让下一个会话按错误的地图去读文件。
但也不要往 code-map 里堆代码片段:它的全部价值在于**便宜**(读一次就能定位)。

---

## 8. 提交前自检(30 秒)

在报告"完成"之前,逐条过:

- [ ] `python checks/size_gate.py` 通过(没有文件超基线)
- [ ] `python checks/arch_gate.py` 通过(依赖方向没破)
- [ ] 改了判据 ⇒ 跑过 `cargo test -p replay` 且结论符合预期
- [ ] 跨层改动 ⇒ 对着 §2 的清单逐处核过
- [ ] 新增/修改的代码若对应某条 T ⇒ 加了 `TODO(Tn)` 锚点
- [ ] `code-map.md` 里对应的那一行改过了
- [ ] 没有留 `dbg!` / `console.log` / 注释掉的代码

**不要在没有跑过门禁的情况下说"完成"。** 说"我改了 X,但没跑 Y,因为 Z"远好于
声称完成而实际没验证。

---

## 9. 环境事实(别重复踩)

> 这些是实测结论,写在这里省得每次重新排查。

### 9.1 Rust 工具链(多机不同,**先 source 再干活**)

这个仓库在多台机器上同步,**工具链位置各不相同**:有的机器装在系统级
(`~/.cargo/bin`,在 PATH 里),有的用项目自带的便携工具链(`.toolchain/`,
不在 PATH 里)。**不要假设 cargo 在 PATH 里,也不要写死任何绝对路径。**

```bash
source scripts/rust-env.sh      # 自动发现并激活;再跑 rust_env_report 看找到哪个
```

发现顺序:系统 PATH → 本项目 `.toolchain/` → **同级目录**的 `.toolchain/`
→ 用户级 `~/.rustup`。全部路径动态推导。

**本机(Windows ASUS 台式,2026-09-29 实测)**:工具链在同级的
`../rpa/.toolchain`(那是 `rpa` 项目自带的),cargo 1.98.1 / rustc 1.98.1。
它**可用于本项目** —— 已实测 `cargo check` / `cargo clippy` / `cargo test`
全部跑通(122 个测试通过)。

⚠️ **`cargo test --workspace` 在本机恒失败**,原因**不是本项目**:
Tauri 依赖链里 `schemars 0.8.22` → `indexmap 1.9.3`,后者在 rustc 1.98 下
编译失败(`E0107: struct takes 3 generic arguments but 2 were supplied`)。
所以**一律用 `-p` 指定 crate**,别用 `--workspace`。CI 也是这么配的。

**另一台机(Intel Mac,2026-10-07 实测)**:工具链在**系统 PATH**
(`~/.cargo/bin`,`stable-x86_64-apple-darwin`),cargo 1.98.1 / rustc 1.98.1,
`cargo check` / `cargo test` 正常。**但 clippy 没装**:

```
error: 'cargo-clippy' is not installed for the toolchain 'stable-x86_64-apple-darwin'
help: run `rustup component add clippy` to install it
```

即 §7 那段④静态检查在**这台机上跑不了**。别把"clippy 没跑"说成"没超基线" ——
规模/架构两道门禁(`checks/` 两个脚本,纯 Python)不受影响,照跑。

### 9.2 换行符(多机同步的坑)

本机 git `core.autocrlf = true`。仓库根有 `.gitattributes` 把 shell 脚本
**钉死为 LF** —— 否则 `.githooks/pre-commit` 检出到 Linux/macOS 后会因
`\r` 报 "bad interpreter"。**改 `.gitattributes` 时注意:它不支持行尾注释**,
`#` 必须独占一行(写了行尾注释会报 "is not a valid attribute name")。

### 9.3 其他

- **Windows 侧没有 `winget` / `scoop` / `choco`**;`reg.exe`、`wmic.exe`、`Add-Type`
  被安全策略屏蔽。读注册表/WMI 用 PowerShell 的 PSDrive + `Get-CimInstance`。
- **PowerShell 工具 stdout 不回显**(本会话实测)。需要输出时重定向到文件再用 Read 读,
  或改用 Bash。
- **禁止从 Bash 调 `powershell.exe`**(会被安全策略拦),必须走 PowerShell 工具。
- **代理** `http://127.0.0.1:10797` 可用。
- **Git Bash 的 `tar` 不认 7z**;要解 7z 用 `C:\Windows\System32\tar.exe` 或 py7zr。
- **`sigma-drift-viz` 在 Windows 上起不来的原因**是这台机器没有 OpenGL 2.0+
  (显卡驱动 Code 43,无厂商 ICD),不是代码问题。详见
  `.workbuddy/memory/2026-09-29.md`。

---

## 10. 一句话总结

**规矩的强度 = 文档的详尽程度 × 执行的自动化程度。**

这个仓库第一项是满分,第二项曾经接近零。上述门禁(`checks/` 两个脚本 + CI +
pre-commit)就是为了把第二项补上。当你在"遵守规矩"和"让这次改动跑通"之间犹豫时,
**先跑门禁** —— 工具会告诉你答案,而不是靠记忆。
