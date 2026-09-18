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

## 标定预览：唯一被允许的只读旁路

`WindowsDesktop::preview()` 是**固有方法，不在 `DesktopPlatform` trait 上**。
它是上面"只能依赖端口"这条规则的唯一例外，理由如下：

- 它不参与业务流程，只服务于界面的区域标定，返回 `(Rect, Screenshot)` 供显示；
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

### 这个命令有两条刻意的约定

**一、不按运行模式设限。** 标定属于**配置**而不是执行：它只读地看一眼目标窗口，
不点击、不输入、不发送，跟本次任务跑演练还是跑真实无关。而且实际使用顺序往往是
"先把窗口和四个区域标定好，再决定用哪种模式跑"，卡在模式上只会让人没法做准备。

**二、窗口类名由调用方传入，不读已保存的配置。** 界面上显示的是**草稿**，
用户改了类名但还没点「保存配置」时配置里仍是旧值。如果这里读配置，就会报出
"界面上明明写着新类名，截图却说找不到窗口"这种自相矛盾的错——调用方传进来的
就是用户此刻看到的值，两边不可能不一致。

## 面向操作者的三个准备命令

这三个命令都只做**准备**，都不属于任务流程，也都**不按运行模式设限**——
它们不点击、不输入、不发送，跟本次任务跑演练还是跑真实无关。

| 命令 | 作用 | 副作用 |
|---|---|---|
| `pick_target_window` | 倒计时内读**光标下**的窗口（类名 / 标题 / exe） | 无（只读） |
| `launch_client` | 启动用户配置的客户端可执行文件 | 启动一个进程 |
| `record_window_geometry` | 只读量出目标窗口尺寸 + DPI，作为标定尺寸 | 无（只读） |

- `pick_target_window` 必须**轻**：倒计时期间每 200ms 采样一次（5 秒 = 25 次），
  所以它**不截屏、不编码**——这是它与 `preview_target_window` 的关键区别，
  后者要截屏 + 缩放 + PNG + base64。`is_self` 用**可执行文件路径**判定，
  不用 PID（PID 每次启动都变）。
- `launch_client` 的 exe 路径与 SHA-256 由界面传入，**不读已保存配置**（同上的草稿理由）。
  路径为空直接拒绝。它**不会**被 `execute()` 调用。
- `record_window_geometry` 走 `WindowsDesktop::measure()`——同样是固有只读方法，
  **不截屏也不聚焦**，因此不会把用户的窗口抢到前台。空类名直接拒绝，
  否则会退化成"匹配任意窗口"。

> 为什么这三个都要做成命令而不是命令行脚本：操作者日常用的是界面。
> 命令行探针（`screen_probe`）只配当诊断工具，用户已明确否掉"让操作者跑脚本"的形态。
