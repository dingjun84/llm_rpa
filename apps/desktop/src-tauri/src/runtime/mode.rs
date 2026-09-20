//! 「这一次跑演练还是真实」这件事本身。
//!
//! 拆成独立文件是因为 `runtime.rs` 破了 `CONVENTIONS.md` §9 的行数基线
//! （见 `docs/todo.md` T17），而这两块是**自成一体的**：纯数据 + 纯函数，
//! 不碰端口、不碰配置的其余部分、不碰装配流程。
//!
//! ⚠️ **别把它当"两个小枚举"看。** [`RuntimeMode`] 上那三个方法承载的是
//! "模式到底影响什么"这件事，而它影响的四件事散在 `runtime.rs` 里：
//! ① 挑哪一组端口；② 真实模式没有标定尺寸就拒绝开跑；③ 审计里的平台字段；
//! ④ 要不要把标定窗口交给核心层。
//! **这四个判断只该有一处**，所以它们留在这个类型自己身上，而不是散到装配流程里。
//!
//! ⚠️ 可见性是刻意的（搬过来时最容易漏的一点）：
//! - `notice()` 被 `lib.rs` 调用 ⇒ `pub`；
//! - `platform_label()` / `calibrated_window()` 只被父模块 `runtime.rs` 调用 ⇒ `pub(super)`。
//!
//! 父模块用 `pub use mode::{DemoScenario, RuntimeMode};` 把它们**再导出**，
//! 所以 `crate::runtime::RuntimeMode` 这条路径照旧成立
//! （`lib.rs` 与 `runtime/tests.rs` 都靠它）。

use automation_core::CalibratedWindow;
use serde::{Deserialize, Serialize};

use super::WindowGeometry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    DryRun,
    Live,
}

impl RuntimeMode {
    /// 核心层审计里那个"平台"字段。
    ///
    /// 演练模式跑的是替身，报 `dry-run`；真实模式报当前操作系统。
    ///
    /// 抽成方法是因为它有**两个调用点**：配置默认值 → 核心层结构的映射
    /// （[`super::RuntimeConfig::to_runner_config`]），以及本次运行参数的覆盖
    /// （[`super::build_runner`]）。写成两处 `match` 的话，改了其中一处就会出现
    /// 「审计里记的是演练、实际跑的是真实」——而那正是审计要防的事。
    pub(super) fn platform_label(self) -> String {
        match self {
            RuntimeMode::DryRun => "dry-run".to_string(),
            RuntimeMode::Live => std::env::consts::OS.to_string(),
        }
    }

    /// 这一次要不要把标定窗口交给核心层。
    ///
    /// 演练模式跑的是替身窗口，**没有**"真实窗口尺寸"这回事：拿配置里的尺寸去比，
    /// 只会把每个演练场景都变成失败。真实模式则必须有——[`super::build_runner`] 在
    /// 装配期先拦一道（没有就直接拒），这里负责把它传下去。
    pub(super) fn calibrated_window(
        self,
        geometry: Option<WindowGeometry>,
    ) -> Option<CalibratedWindow> {
        match self {
            RuntimeMode::DryRun => None,
            RuntimeMode::Live => geometry.map(|geometry| CalibratedWindow {
                width: geometry.width,
                height: geometry.height,
                scale_factor: geometry.scale_factor,
            }),
        }
    }

    /// 给操作者看的一句话结论。
    ///
    /// 由后端出文案、前端按**界面上当前选的那个模式**去取，而不是前端自己拼一遍：
    /// 同一句话维护两处必然漂移，而漂移的方向恰好是最危险的那个——
    /// 提示说"演练"、实际在动真窗口。
    ///
    /// 也**不能**只下发"当前模式那一句"：模式现在是运行参数，界面上选的与配置里
    /// 存的可以不是同一个，只发一句就得选一边，选错就是上面那个方向。
    pub fn notice(self) -> &'static str {
        match self {
            RuntimeMode::DryRun => {
                "当前为演练模式：全部使用替身端口，不会启动企业微信、不会产生任何真实输入。"
            }
            RuntimeMode::Live => {
                "当前为真实模式：会操作本机企业微信窗口。发送期间请勿切换窗口或操作鼠标键盘。"
            }
        }
    }
}

/// 两种模式各自那句给操作者看的提示。
///
/// ★ 为什么不只发"当前模式那一句"：**模式现在是运行参数**，界面上选的与配置里
/// 存的可以不是同一个（界面上切了模式、没点保存）。只发一句就必须在两边里选一个，
/// 而选错的方向恰好是最危险的那个——提示说"演练"、实际在动真窗口。
/// 发一对照表，前端按**界面上当前选的那个模式**去取，显示的与真正会跑的
/// 就不可能不一致。
///
/// 文案本身仍只有一处：[`RuntimeMode::notice`] —— 这里只是把它**成对**取出来。
/// 放在本文件而不是 `lib.rs`，理由和 `RuntimeMode` 一样：它是"两种模式各自是什么"
/// 这件事的一部分，不是命令层的组装逻辑。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeNotices {
    pub dry_run: String,
    pub live: String,
}

impl ModeNotices {
    /// 两种模式各取一句。调用点在 `lib.rs` 的 `runtime_info` 命令。
    pub fn all() -> Self {
        Self {
            dry_run: RuntimeMode::DryRun.notice().to_string(),
            live: RuntimeMode::Live.notice().to_string(),
        }
    }
}

/// 演练模式下要复现的场景，用于在界面上演示各类失败路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DemoScenario {
    Happy,
    DuplicateContact,
    NearName,
    LowConfidence,
    HeaderMismatch,
    LoginPrompt,
    DeliveryMissing,
    UnstableScreen,
}
