//! 与平台无关的任务状态机、端口契约与编排器。
//!
//! 此 crate 不执行屏幕捕获、OCR 或系统输入；所有外部能力都通过
//! [`ports`] 中的 trait 注入，因此可以在没有真实桌面的环境下完整测试。

pub mod audit;
pub mod policy;
pub mod ports;
pub mod regions;
pub mod runner;
pub mod state;

pub use audit::*;
pub use policy::*;
pub use ports::*;
pub use regions::*;
pub use runner::*;
pub use state::*;
