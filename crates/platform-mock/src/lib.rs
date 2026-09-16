//! 仅用于测试工作流，不会捕获屏幕或生成任何真实输入事件。
//!
//! 本 crate 覆盖全部四个端口（平台、OCR、联系人匹配、人工确认），
//! 因此 `automation-core` 的端到端测试可以完全脱离真实桌面运行。

pub mod confirmation;
pub mod desktop;
pub mod fault;
pub mod scenario;
pub mod vision;

pub use confirmation::{ConfirmationOutcome, MockHumanConfirmation};
pub use desktop::{MockDesktop, DEFAULT_METRICS, DEFAULT_WINDOW};
pub use fault::{Fault, MockFaults};
pub use scenario::{tb, MockScenario};
pub use vision::{MockContactMatcher, MockOcr, ScriptedCall};
