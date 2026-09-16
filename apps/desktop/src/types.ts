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
  launching_client: "启动企业微信",
  waiting_for_client: "等待客户端就绪",
  searching_contact: "查找联系人",
  verifying_candidate: "核验联系人",
  verifying_chat_header: "核验聊天页标题",
  preparing_message: "准备消息",
  awaiting_human_confirmation: "等待人工确认",
  sending: "发送",
  verifying_delivery: "核验送达",
  completed: "已完成",
  needs_human_review: "需要人工处理",
  failed: "已失败",
  cancelled: "已取消",
};

export const TERMINAL_STATES: TaskState[] = [
  "completed",
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
  confirmation_ttl_secs: number;
  min_confidence: number;
  regions: RegionConfig;
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
  width: number;
  height: number;
  /** `data:image/png;base64,...`，可直接作为 `<img src>`。 */
  image: string;
  fingerprint: string;
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
