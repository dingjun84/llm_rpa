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
    fn launch_wecom(&self) -> Result<(), AutomationError>;

    /// 将已验证的企业微信窗口置于前台，并返回它在屏幕上的边界。
    fn focus_wecom(&self) -> Result<Rect, AutomationError>;

    /// 捕获指定屏幕区域。调用方不得传入企业微信窗口外的区域。
    fn capture(&self, region: Rect) -> Result<Screenshot, AutomationError>;

    /// 点击前验证当前前台窗口与预期窗口一致；不满足则拒绝输入。
    fn guarded_click(&self, target: Point, expected_window: Rect) -> Result<(), AutomationError>;

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
