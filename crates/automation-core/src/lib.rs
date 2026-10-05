//! 与平台无关的任务状态机、端口契约与编排器。
//!
//! 此 crate 不执行屏幕捕获、OCR 或系统输入；所有外部能力都通过
//! [`ports`] 中的 trait 注入，因此可以在没有真实桌面的环境下完整测试。

pub mod audit;
pub mod candidates;
pub mod ocr_batch;
pub mod diagnostics;
/// 搜索下拉挑人这条判据。公开出来是为了**离线重放**能拿同一份判据重跑
/// （见 `docs/todo.md` T29 与 `tools/replay`）——判据藏在编排器私有方法里，
/// 重放就只能自己再写一遍，那正是"两套判据"的开始。
pub mod dropdown;
pub mod policy;
pub mod ports;
pub mod task;
pub mod yolo;
pub mod regions;
pub mod runner;
pub mod state;

pub use audit::*;
pub use ocr_batch::{recognize_many, recognize_many_with_raw, recognize_many_with_raw_retrying, MAX_OCR_CONCURRENCY};
pub use diagnostics::*;
pub use policy::*;
pub use ports::*;
pub use task::*;
pub use yolo::{shot_point_to_screen, YoloDetection, YoloDetector};
pub use regions::*;
pub use runner::*;
pub use state::*;
