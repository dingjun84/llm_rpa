//! 仅用于测试工作流，不会捕获屏幕或生成任何真实输入事件。
//!
//! 本 crate 覆盖全部四个端口（平台、OCR、联系人匹配、图标定位），
//! 因此 `automation-core` 的端到端测试可以完全脱离真实桌面运行。
//!
//! 这里曾经还有一个"人工确认"替身（`MockHumanConfirmation`）：2026-09-22 取消
//! 人工确认后一起删掉。**不要为了"让老用例编译得过"把它留成空实现**——
//! 那会让"发送前等过一次确认"这件事在测试里继续看起来成立。

pub mod desktop;
pub mod fault;
pub mod icons;
pub mod scenario;
pub mod vision;

pub use desktop::{MockDesktop, DEFAULT_METRICS, DEFAULT_WINDOW};
pub use fault::{Fault, MockFaults};
pub use icons::MockIconLocator;
pub use scenario::{tb, MockScenario};
pub use vision::{MockContactMatcher, MockOcr, ScriptedCall};
