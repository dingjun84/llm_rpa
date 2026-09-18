export type TaskState =
  | "draft"
  | "launching_client"
  | "waiting_for_client"
  | "searching_contact"
  | "verifying_candidate"
  | "verifying_chat_header"
  | "preparing_message"
  | "awaiting_human_confirmation"
  | "sending"
  | "verifying_delivery"
  | "completed"
  | "prepared"
  | "needs_human_review"
  | "failed"
  | "cancelled";

export const HAPPY_PATH: TaskState[] = [
  "draft",
  "launching_client",
  "waiting_for_client",
  "searching_contact",
  "verifying_candidate",
  "verifying_chat_header",
  "preparing_message",
  "awaiting_human_confirmation",
  "sending",
  "verifying_delivery",
  "completed",
];

export const STATE_LABELS: Record<TaskState, string> = {
  draft: "草稿",
  // 客户端由操作者自己启动并登录，任务只负责接管已经就绪的窗口。
  // 标识仍是 `launching_client`（审计记录按它落库，改了就读不回来），只是文案改了。
  launching_client: "接入客户端",
  waiting_for_client: "客户端已就绪",
  searching_contact: "查找联系人",
  verifying_candidate: "核验联系人",
  verifying_chat_header: "核验聊天页标题",
  preparing_message: "准备消息",
  awaiting_human_confirmation: "等待人工确认",
  sending: "发送",
  verifying_delivery: "核验送达",
  completed: "已完成",
  prepared: "已填入正文，未发送",
  needs_human_review: "需要人工处理",
  failed: "已失败",
  cancelled: "已取消",
};

/**
 * 终态。
 *
 * `prepared` 也是终态，但和 `completed` 一样属于**正常结束**：
 * 它表示"按配置在发送前停下"，没有发生任何意外，只是没发出去。
 */
export const TERMINAL_STATES: TaskState[] = [
  "completed",
  "prepared",
  "needs_human_review",
  "failed",
  "cancelled",
];

export interface Failure {
  code: string;
  reason: string;
}

export interface HistoryEntry {
  from: TaskState;
  to: TaskState;
  at_ms: number;
  detail: string | null;
}

export interface EvidenceView {
  label: string;
  byte_len: number;
  sha256: string;
}

export interface TaskView {
  id: string;
  external_contact_name: string;
  text: string;
  text_length: number;
  created_by: string;
  state: TaskState;
  state_label: string;
  detail: string | null;
  failure: Failure | null;
  evidence: string[];
  history: HistoryEntry[];
  awaiting_confirmation: boolean;
  evidence_artifacts: EvidenceView[];
}

export type RuntimeMode = "dry_run" | "live";

export type DemoScenario =
  | "happy"
  | "duplicate_contact"
  | "near_name"
  | "low_confidence"
  | "header_mismatch"
  | "login_prompt"
  | "delivery_missing"
  | "unstable_screen";

export const SCENARIO_LABELS: Record<DemoScenario, string> = {
  happy: "正常发送（主路径）",
  duplicate_contact: "同名联系人（拒绝猜测）",
  near_name: "只有近似名（拒绝匹配）",
  low_confidence: "置信度不足（拒绝点击）",
  header_mismatch: "聊天页标题不符（双重核验失败）",
  login_prompt: "出现登录/风控提示（停止任务）",
  delivery_missing: "聊天区未出现消息（不判定送达）",
  unstable_screen: "发送后界面无变化（不判定送达）",
};

export interface RegionConfig {
  contact_panel: [number, number, number, number];
  chat_header: [number, number, number, number];
  chat_body: [number, number, number, number];
  composer: [number, number, number, number];
}

export interface RuntimeConfig {
  mode: RuntimeMode;
  demo_scenario: DemoScenario;
  wecom_exe: string | null;
  wecom_exe_sha256: string | null;
  window_class: string;
  ocr_command: string | null;
  ocr_args: string[];
  ocr_timeout_ms: number;
  /**
   * 单步超时的下限（秒）。
   *
   * 实际生效值会与「OCR 超时 + 5 秒余量」取较大者，所以调大 OCR 超时不会被它悄悄截断。
   */
  step_timeout_secs: number;
  confirmation_ttl_secs: number;
  min_confidence: number;
  regions: RegionConfig;
  /** 「只填不发」：正文填进输入框后就结束，绝不发送。 */
  stop_before_send: boolean;
  /** 查找联系人时最多向下滚动多少次。 */
  max_scroll_attempts: number;
  /** 每次向下滚动的格数。 */
  scroll_notches_per_step: number;
  /**
   * 滚动时鼠标落在联系人列表内的哪个位置（相对该区域的比例，0–1）。
   *
   * 默认 `(0.62, 0.5)` = 上下居中、左右偏右一点。
   * 不用区域中心的原因：联系人候选区的左边界是 0.0，把左侧导航图标栏和头像列
   * 一起圈了进来，水平中心恰好压在头像列上。
   */
  scroll_anchor: { x: number; y: number };
  /**
   * 标定时记录的窗口几何。
   *
   * 客户端由操作者手动启动并登录，任务只在**这个尺寸**下工作：尺寸对不上就转人工，
   * 绝不按错的尺寸去点。真实模式下这一项必须有值，否则任务会被拒绝装配。
   */
  calibrated_window: WindowGeometry | null;
  /**
   * 联系人列表最多完整扫描几轮（每轮 = 从列表顶部向下扫到底）。
   *
   * 列表按「最近有消息」排序，扫描期间到达的新消息会把目标顶到最上面，
   * 而那一屏早被翻过去了。多扫一轮就是专门兜这种情况的。
   */
  max_search_sweeps: number;
  /** 是否在「该动的画面没动」时判定客户端卡死并转人工。 */
  liveness_check: boolean;
  /**
   * 滚动之后**等画面停稳**再截图的上限（毫秒），`0` = 不等。
   *
   * 这是**上限**而不是等待时长：画面一稳就立刻继续，正常只多截一帧。
   * 客户端列表滚动带缓动，滚完立刻截图会截到中间帧（文字糊、行错位），
   * 还会让「滚了没动 ⇒ 到底了」把「还没画完」当成「到底了」。
   */
  scroll_settle_ms: number;
  /**
   * 是否把每轮 OCR 实际读到的文字写进任务日志与界面。
   *
   * 排查「找不到联系人」时只有指纹看不出问题——分不清是 OCR 读错了，
   * 还是名字根本不在这屏。只写日志与界面，不进审计库。
   */
  log_ocr_candidates: boolean;
  /**
   * ⚠️ **临时放宽姓名匹配**：精确匹配不到时，退化为「候选文本包含目标名」。
   *
   * 违反 `docs/architecture.md` 要求的逐字精确匹配，会把「找不到人」
   * 变成「可能找错人」。**有真实发送需求时必须关掉。** 待办见 `docs/todo.md`。
   * 只影响真实模式。
   */
  relaxed_name_match: boolean;
}

/** 标定时记录的窗口几何（屏幕坐标 + 显示器缩放）。 */
export interface WindowGeometry {
  x: number;
  y: number;
  width: number;
  height: number;
  scale_factor: number;
}

export interface RuntimeInfo {
  config: RuntimeConfig;
  data_dir: string;
  audit_entry_count: number;
  is_windows: boolean;
  notice: string;
}

/** 屏幕坐标系下的矩形（像素）。 */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** 目标窗口的一次只读预览，用于区域标定。 */
export interface WindowPreview {
  window: Rect;
  /**
   * 预览图的像素尺寸。窗口过宽时后端会等比缩小，
   * 因此不一定等于窗口尺寸——要窗口真实尺寸请用 `window`。
   */
  width: number;
  height: number;
  /** `data:image/png;base64,...`，可直接作为 `<img src>`。 */
  image: string;
  fingerprint: string;
}

/** 光标下的一次窗口快照，用于界面「指认目标窗口」。 */
export interface PickedWindow {
  /** 窗口类名。可以直接填进 `RuntimeConfig.window_class`。 */
  class_name: string;
  /** 窗口标题。目前只用于展示与人工核对，不参与匹配。 */
  title: string;
  /** 窗口所属进程的可执行文件路径；读不到时为 `null`。 */
  exe_path: string | null;
  /** 窗口在屏幕上的边界（像素）。 */
  window: Rect;
  /** 这个窗口是不是本程序自己。 */
  is_self: boolean;
}

export interface StartTaskRequest {
  external_contact_name: string;
  text: string;
  created_by?: string | null;
}

export interface ConfirmationRequest {
  task_id: string;
  external_contact_name: string;
  text: string;
  expires_in_ms: number;
}
