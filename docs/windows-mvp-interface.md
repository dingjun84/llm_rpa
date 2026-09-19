# Windows MVP：平台与视觉接口契约

本文定义实现阶段的 Rust 端口。业务流程只能依赖这些端口，不能直接调用 Win32、OCR SDK 或鼠标键盘库。

## 公共类型

```rust
pub type TaskId = uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point { pub x: i32, pub y: i32 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect { pub x: i32, pub y: i32, pub width: i32, pub height: i32 }

#[derive(Debug, Clone)]
pub struct Screenshot {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub captured_at: std::time::SystemTime,
    pub fingerprint: String,
}

#[derive(Debug, Clone)]
pub struct TextBox {
    pub text: String,
    pub bounds: Rect,
    pub confidence: f32,
}

/// 一张**由操作者自己框出来**的图标模板（BGRA，行优先，与 `Screenshot` 同一套表示）。
///
/// 它来自**图标库**（`<数据目录>/icons/<名字>.png`），`label` 就是那个名字。
/// 程序绝不自己裁一块"看起来像图标"的区域：猜错位置、裁到空白，
/// 都会变成一次**静默的、看起来完全正常**的运行——匹配分数照样很高
/// （它匹配的就是它自己刚裁的那块），点击照样发出去，只是点到了别的地方。
#[derive(Debug, Clone)]
pub struct IconTemplate {
    pub label: String,
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// 一次图标命中的位置与分数。`bounds` 是**图像坐标系**，由调用方换算成屏幕坐标。
#[derive(Debug, Clone, PartialEq)]
pub struct IconMatch {
    pub bounds: Rect,
    pub score: f32,
    pub template_index: usize,
    pub template_label: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error("企业微信窗口不可用或不是前台窗口")]
    ClientNotReady,
    #[error("屏幕状态在操作前发生变化")]
    ScreenChanged,
    #[error("本地视觉识别结果不确定：{0}")]
    AmbiguousVision(String),
    #[error("需要人工处理：{0}")]
    NeedsHumanReview(String),
    #[error("平台操作失败：{0}")]
    Platform(String),
}
```

## 平台端口

```rust
/// 只允许操作用户当前可见、已解锁的交互式桌面。
pub trait DesktopPlatform: Send + Sync {
    /// 启动已由用户配置且经验证的企业微信可执行文件；不得猜测路径或提权启动。
    ///
    /// **不属于任务流程**：客户端由操作者自己启动并登录，`execute()` 不会调用它。
    /// 它只服务于界面上的「启动客户端」按钮。
    fn launch_wecom(&self) -> Result<(), AutomationError>;

    /// 将已验证的企业微信窗口置于前台，并返回它在屏幕上的边界。
    ///
    /// 这是任务开始时的「接管」动作：找不到可见窗口即返回 `ClientNotReady`，
    /// 由核心层转成 `NeedsHumanReview`，绝不自动拉起客户端。
    fn focus_wecom(&self) -> Result<Rect, AutomationError>;

    /// 捕获指定屏幕区域。调用方不得传入企业微信窗口外的区域。
    fn capture(&self, region: Rect) -> Result<Screenshot, AutomationError>;

    /// 点击前验证当前前台窗口与预期窗口一致；不满足则拒绝输入。
    fn guarded_click(&self, target: Point, expected_window: Rect) -> Result<(), AutomationError>;

    /// 在 `at` 处滚动鼠标滚轮。`notches > 0` 表示向下滚动内容。
    ///
    /// 滚轮事件只送给**光标下**的窗口，所以实现方必须先把光标移到 `at`，
    /// 并和点击一样在动作前验证前台窗口与标定一致。
    fn scroll(&self, at: Point, notches: i32, expected_window: Rect)
        -> Result<(), AutomationError>;

    /// 目标窗口所属线程是否仍在处理消息。`false` 表示它已卡死。
    ///
    /// Windows 侧实现为 `IsHungAppWindow`（窗口线程超过 5 秒未取消息即判定未响应）。
    /// 这是**纯只读查询**，不改焦点、不产生输入，因此不受前台守卫约束。
    /// 尚未定位到窗口时返回 `ClientNotReady`。
    ///
    /// 它与"画面有没有变化"是互补的两条判据：画面比对抓不住
    /// "主线程死锁但界面仍在刷新"之外的边角情形，系统级判据也抓不住
    /// "线程活着但滚不动"的假死，两条都要。
    fn is_responsive(&self) -> Result<bool, AutomationError>;

    /// 将文本写入剪贴板并粘贴到当前已聚焦控件；完成后清除临时剪贴板内容。
    fn paste_text(&self, text: &str, expected_window: Rect) -> Result<(), AutomationError>;

    /// 发送由配置限定的快捷键；不支持任意按键序列。
    fn send_message_shortcut(&self, expected_window: Rect) -> Result<(), AutomationError>;
}
```

## 视觉端口

```rust
pub trait LocalOcr: Send + Sync {
    /// 仅对内存中的局部截图推理，禁止网络上传与远程推理。
    fn recognize(&self, image: &Screenshot) -> Result<Vec<TextBox>, AutomationError>;
}

pub trait ContactMatcher: Send + Sync {
    /// 返回唯一且满足最低置信度的完全匹配项；同名或模糊匹配必须返回错误。
    fn find_unique_exact_match(
        &self,
        expected_name: &str,
        candidates: &[TextBox],
        min_confidence: f32,
    ) -> Result<TextBox, AutomationError>;

    /// 「这一个候选行不行」。与上面的「在候选集里挑一个」必须对同一个名字给同样的答案。
    fn accepts(&self, expected_name: &str, candidate: &TextBox) -> bool;
}

/// 图标定位端口：在一帧**局部截图**里用模板匹配找出一个小图的位置。
///
/// 与 `LocalOcr` 的分工很清楚——OCR 回答"这一片文字写的是什么"，
/// 本端口回答"这个图标在哪儿"。
pub trait IconLocator: Send + Sync {
    /// `templates` 为空 ⇒ **必须报错**，不得当成"没找到"静默通过。
    /// 最高分低于 `min_score` ⇒ `AmbiguousVision`（转人工），
    /// 不得返回一个"分数不高但先用了"的结果。
    fn locate(
        &self,
        frame: &Screenshot,
        templates: &[IconTemplate],
        min_score: f32,
    ) -> Result<IconMatch, AutomationError>;
}
```

## 工作流输入与确认端口

```rust
pub struct SendTask {
    pub id: TaskId,
    pub external_contact_name: String,
    pub text: String,
    pub created_by: String,
}

pub trait HumanConfirmation: Send + Sync {
    /// 确认界面必须同时显示目标名称与消息预览，并给确认设置过期时间。
    fn confirm_send(&self, task: &SendTask, expires_in: std::time::Duration)
        -> Result<(), AutomationError>;
}
```

## 实现禁止项

- 不得使用客户端内存读取、注入、Hook 或逆向接口；
- 不得在 `DesktopPlatform` 内隐藏网络发送、定时发送或重试发送；
- 不得根据相似度“猜测”联系人；
- 不得绕过权限提示、验证码、登录或风控限制；
- 不得把 `Screenshot`、`TextBox`、任务正文上传到远程服务。

## 标定预览：只读旁路之一

`WindowsDesktop::preview()` 是**固有方法，不在 `DesktopPlatform` trait 上**。
它是上面"只能依赖端口"这条规则的例外，理由如下：

- 它不参与业务流程，只服务于界面的标定（区域、图标模板），返回 `(Rect, Screenshot)` 供显示；
- 它**不改变焦点**——与 `focus_wecom` 的关键区别。标定只需要"看一眼窗口长什么样"，
  而 `SetForegroundWindow` 在 Windows 前台锁定策略下经常被拒绝（调用方自身不在前台时），
  做成"先聚焦再截图"会让这个功能时灵时不灵；
- 它不点击、不粘贴、不发送、不写剪贴板，因此不产生任何对外副作用；
- 它仍然受资源上限约束（`preview_max_pixels`），窗口矩形是外部数据，
  不能拿它直接分配内存。

把它放进 trait 会迫使每个平台适配器（含 mock）都去实现一个与业务流程无关的
展示用方法，得不偿失。IPC 命令层本来就是组合根，允许按平台条件编译直接引用
`platform-windows`。

送进界面前会等比缩到 `PREVIEW_MAX_WIDTH`：区域叠加层用的是百分比，
与图像实际像素尺寸无关，所以缩小不影响标定精度，却能把 4K 窗口的
IPC 载荷从几十 MB 压到几百 KB。

> ⚠️ **缩过的图只能给人看，不能当模板。** 图标模板必须回到原始分辨率去裁
> （见下面「图标库」一节），所以 `save_icon_from_crop` 会按框**重新截一张**，
> 而不是从预览图上裁一块下来。

### 这些命令有两条刻意的约定

**一、不按运行模式设限。** 标定属于**配置**而不是执行：它只读地看一眼目标窗口，
不点击、不输入、不发送，跟本次任务跑演练还是跑真实无关。而且实际使用顺序往往是
"先把窗口和四个区域标定好，再决定用哪种模式跑"，卡在模式上只会让人没法做准备。
（`click_icon` 会产生真实点击，但它同样不设限——它是**人按的按钮**，
不是任务流程的一步。界面上用两次点击确认来兜住这一点。）

**二、窗口类名由调用方传入，不读已保存的配置。** 界面上显示的是**草稿**，
用户改了类名但还没点「保存配置」时配置里仍是旧值。如果这里读配置，就会报出
"界面上明明写着新类名，截图却说找不到窗口"这种自相矛盾的错——调用方传进来的
就是用户此刻看到的值，两边不可能不一致。

## 面向操作者的准备命令

这些命令都只做**准备**，都不属于任务流程。它们分成两类。

### 只读的（不点击、不输入、不发送）

| 命令 | 作用 | 副作用 |
|---|---|---|
| `pick_target_window` | 倒计时内读**光标下**的窗口（类名 / 标题 / exe） | 无 |
| `record_window_geometry` | 只读量出目标窗口尺寸 + DPI，作为标定尺寸 | 无 |
| `preview_target_window` | 截一张窗口预览图，供区域标定与图标框选 | 无 |
| `probe_nav_icon` | 在当前画面上试一次图标模板匹配，报出**分数与位置** | 无 |
| `list_icons` | 列出图标库里的图标（名字、尺寸、缩略图） | 无 |
| `save_icon_from_crop` | 把预览图上框出来的一块存成图标模板 | 无 |

只读的那一组**不按运行模式设限**：它们跟这次任务跑演练还是跑真实无关，
实际使用顺序也往往是"先把窗口、区域、图标都标定好，再决定用哪种模式跑"。

### 会产生副作用的（由人明确按下）

| 命令 | 作用 | 副作用 |
|---|---|---|
| `launch_client` | 启动用户配置的客户端可执行文件 | 启动一个进程 |
| `delete_icon` | 删除图标库里的一张图标 | 删一个文件 |
| `click_icon` | 定位一个图标并**真的点它一下** | **产生鼠标点击** |

这三个同样**不按运行模式设限**：运行模式决定的是"任务怎么执行"，
不是"人能不能动手"。但界面上必须写明它会真的产生输入——`click_icon`
的按钮做成**两次点击确认**（第一次"上膛"、第二次才真的点），
就是因为它会在对方的聊天客户端上产生真实动作。

- `pick_target_window` 必须**轻**：倒计时期间每 200ms 采样一次（5 秒 = 25 次），
  所以它**不截屏、不编码**——这是它与 `preview_target_window` 的关键区别，
  后者要截屏 + 缩放 + PNG + base64。`is_self` 用**可执行文件路径**判定，
  不用 PID（PID 每次启动都变）。
- `launch_client` 的 exe 路径与 SHA-256 由界面传入，**不读已保存配置**（同上的草稿理由）。
  路径为空直接拒绝。它**不会**被 `execute()` 调用。
- `record_window_geometry` 走 `WindowsDesktop::measure()`——同样是固有只读方法，
  **不截屏也不聚焦**，因此不会把用户的窗口抢到前台。空类名直接拒绝，
  否则会退化成"匹配任意窗口"。
- `probe_nav_icon` 与 `save_icon_from_crop` 都取**草稿**参数，并且
  **模板载入走的是与任务装配同一个函数**——否则按钮报出来的分数与任务里
  真正会用的那张图不一致，标定就白做了。

## 图标库

图标库里存的是「从真实画面上框出来的一个小图标」，一个图标一个**名字**
（`<数据目录>/icons/<名字>.png`）。它是导航图标那一步的**前提**：
图标上没有文字，OCR 读不到，只能靠模板匹配，而模板只能由人框出来。

### 名字为什么要卡得这么严

名字会被拼成文件名，所以 `icon_library::validate_name` 拒绝：

- **路径分隔符与保留字符**——名字一旦能带 `\` 或 `..`，「保存图标」就变成了
  「往任意路径写文件」；
- **结尾的点**——Windows 建文件时会**静默**去掉它，于是"保存成功"之后
  按原名再也找不到那个文件，症状是「明明存了却说没有」；
- **保留设备名**（`CON` / `NUL` / `COM1` …）——带上扩展名也建不出来，
  报出来的是没头没尾的「系统找不到指定的文件」。

规则只在**一处**实现，命令与界面都走它。

### 为什么框选之后要重新截一张

`save_icon_from_crop` 收的是**预览图坐标系**里的框（`rect` + `preview`），
但它**不从预览图上裁**：预览是缩放过的（Triangle 滤波），裁出来的模板
边缘会带上插值出来的杂色，拿去匹配原始分辨率的真实画面自然对不准——
而且分数不会低到让人起疑，只表现为「图标明明在，就是匹配不上」。
所以它按框重新截一张原始分辨率的窗口画面再裁。

代价是必须确认两次截图之间窗口尺寸没变（否则换算比例失效、框会落到别处）。
判据用的是**同一个缩放函数**算出来的期望尺寸，不是另写一份比例公式。

写完立刻用任务装配时的同一个载入函数验一遍；不可用就**把文件删掉**再报错——
图标库里只允许留能用的模板。留下的坏模板会以"分数很低"的形式在任务里发作，
而那时人只会去怀疑阈值。

### `probe_nav_icon` 为什么绕过阈值

`IconLocator::locate` 把"分数不够"变成一个错误（`AmbiguousVision`），这对任务是**对的**：
任务只需要"过/不过"。但标定需要的恰恰是**分数本身**——"0.62 差多少""哪张模板分高"
这些信息在"过/不过"的结论里全丢了。所以这个命令直接调 `vision::match_template`，
把分数、位置、命中的是哪张模板一起报出来，并附一个 `accepted` 表示是否过阈值。

**没过阈值也照样返回位置**，这是刻意的：那个位置是"它认为最像的地方"，
对着预览图看一眼就知道是模板截错了还是搜索区没盖住图标。只报"没找到"的话，
操作者手里就什么线索都没有。

这是**唯一**一处绕过判据的地方，且它只读、不参与任务流程。
编排层仍然只问 `IconLocator`，判据仍然只有一处。

`click_icon` 则相反：它**硬走 `IconLocator`**，分数不够直接报错且**一次点击都不发**。
这个按钮问的是"这张模板能不能用"，"分数不够但先点了"正好是最不能接受的结果。

### `click_icon` 的判据与任务里那一步**刻意不同**

同一个动作（点导航图标），但结论不一样：

- **任务里**（`NavigatingToView`）：点击后画面没变只记一条警告，**继续往下走**。
  因为"界面本来就停在这个视图上"与"点击真的没生效"在画面上分不出来，
  而前者是最常见的场景（上次跑完就留在这个视图上）⇒ 硬失败会变成"第二次跑必然失败"。
  判定交给下一步只读的 `locate_contact`。详见 `docs/todo.md` T11。
- **这里**：如实回报 `changed`，不做任何收敛。按按钮的人要的就是
  "这一下到底有没有生效"，把两种可能都写进 `notice` 让他自己判断。

> 为什么这些都要做成命令而不是命令行脚本：操作者日常用的是界面。
> 命令行探针（`screen_probe`）只配当诊断工具，用户已明确否掉"让操作者跑脚本"的形态。
>
> 探针里对应的是 `screen_probe template`（截图标模板）与 `screen_probe findicon`
> （量分数）——它们是**排查用**的，界面上的按钮才是操作者的日常路径。
