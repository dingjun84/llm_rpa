export type TaskState =
  | "draft"
  | "launching_client"
  | "waiting_for_client"
  | "navigating_to_view"
  | "navigated"
  | "searching_contact"
  | "verifying_candidate"
  | "verifying_profile"
  | "opening_chat_from_profile"
  | "verifying_chat_header"
  | "preparing_message"
  | "sending"
  | "verifying_delivery"
  | "completed"
  | "prepared"
  | "needs_human_review"
  | "failed"
  | "cancelled";

/**
 * 时间线上要展示的状态顺序。
 *
 * ⚠️ 它是**两条工作流的并集**，不是某一条的路径：搜索式会多走
 * `verifying_profile` → `opening_chat_from_profile`（点下拉那一行落在资料页上，
 * 还要从资料页点一次「发消息」），列表扫描式则直接进聊天页。
 * 所以"哪一步走过了"必须按**实际轨迹**判断，不能按"下标小于当前"推——
 * 后者会把列表式从没走过的资料页两步也标成"已完成"。
 *
 * 并集的相对顺序对两条路都成立：两条路在 `verifying_candidate` 之后分岔，
 * 在 `verifying_chat_header` 之前汇合。
 */
export const HAPPY_PATH: TaskState[] = [
  "draft",
  "launching_client",
  "waiting_for_client",
  // 可选的一步：用模板匹配点左侧导航图标把视图切过去。
  // 它**不是**必经状态（开关关着时会直接跳到 searching_contact），
  // 放在这里只是为了让主路径时间线能完整展示它。
  "navigating_to_view",
  "searching_contact",
  "verifying_candidate",
  // 只有搜索式会经过这两步。
  "verifying_profile",
  "opening_chat_from_profile",
  "verifying_chat_header",
  "preparing_message",
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
  navigating_to_view: "切换视图",
  // 「只做导航」那条路的终态：找到图标、点它，到此为止。
  // 它不是"走完了整条流程"，所以与 `completed` 分开。
  navigated: "已切换视图",
  searching_contact: "查找联系人",
  verifying_candidate: "核验联系人",
  verifying_profile: "核验资料页",
  opening_chat_from_profile: "从资料页进入聊天",
  verifying_chat_header: "核验聊天页标题",
  preparing_message: "准备消息",
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
 *
 * `navigated` 同理：它是「只做导航」那条路的正常结束，只是那条路本来就不发送。
 */
export const TERMINAL_STATES: TaskState[] = [
  "completed",
  "prepared",
  "navigated",
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
  evidence_artifacts: EvidenceView[];
  /** 过程日志路径；重启后从日志恢复的条目也会带上。 */
  log_path?: string | null;
  /** 是否从磁盘日志恢复（非本进程内存任务）。 */
  from_log?: boolean;
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

/**
 * 一次区域标定的结果。
 *
 * 存的是**相对窗口的比例**，不是像素：窗口挪动、换分辨率都不用重标。
 * ⚠️ 但**窗口尺寸变了要重标**——界面元素（左侧图标栏、头像列）是固定像素宽的，
 * 同一份比例在另一个尺寸下会落到别的地方（见 `docs/todo.md` T2）。
 * 所以每一项都记下 `window`，用来回答"这一份比例是在多大的窗口上量的"。
 */
export interface AreaMark {
  rect: [number, number, number, number];
  calibrated_at_ms: number;
  /** 标定时目标窗口的几何。位置只用于显示，不参与判断。 */
  window: Rect;
}

/** 标定计划里的一个界面状态。一次只引导标一个。 */
export interface CalibrationScene {
  id: string;
  label: string;
  /** 「现在请把客户端切成什么样」。决定了截图里有没有要标的东西。 */
  instruction: string;
  items: CalibrationItem[];
}

/** 一个标定项。清单由后端下发，前端不自己写一份。 */
export interface CalibrationItem {
  key: string;
  label: string;
  hint: string;
  /** 真实模式下缺了它就跑不起来的项。 */
  required: boolean;
  /**
   * 坐标存在草稿的哪个字段。
   *
   * **由后端下发**（`calibration::ITEMS` 里的 `storage`），前端不自己判断——
   * 这个映射写成两份的话，迟早有一项会写进一个没人读的地方，
   * 而症状是「框明明拖了，任务里却用不上」。
   */
  storage: "regions" | "area_marks";
  /**
   * 写进 `regions` 时该用的**字段名**。只有 `storage === "regions"` 的项才有。
   *
   * ★★ 必须用它当键，**不能用 `key`** —— 两者不一定相同：
   * 「列表区」的 key 是 `list_area`，存的却是 `regions.contact_panel`。
   * 拿 `key` 当字段名的话，框会写进一个配置里不存在的键，后端 serde
   * 反序列化时**静默丢掉**：不报错、不警告，症状只是
   * 「标完、保存，再打开就没了」——用户 2026-09-19 实测报的就是这个。
   *
   * 权威定义在后端 `CoreRegion::field_name`（只此一处）。
   */
  region_field: string | null;
  /** 当前坐标（比例）。`null` = **还没标定**，不要用默认值把它填上。 */
  rect: [number, number, number, number] | null;
  /** 标定元数据。只有 `area_marks` 存储的项才有——`regions` 里存不下这些。 */
  mark: AreaMark | null;
  /**
   * 命令行探针截图（`screen_probe annotate`）上的角标编号 `1`~`4`。
   *
   * 只有 `regions` 那四项有值。它与界面上的清单编号（`2.3` 这种）**是两套**：
   * 四个核心区域分散在不同场景里，编号对不上。两个都显示出来，
   * 操作者按截图说「把 3 往左挪」时才不会指错。
   */
  probe_index: number | null;
}

export interface CalibrationPlan {
  scenes: CalibrationScene[];
  marked_count: number;
  total_count: number;
  /**
   * 配置里存着、但**清单里已经没有**的标定项（key 已按字典序排好）。
   *
   * 它们不会被任何流程读到，**却会挡住保存**——后端落盘前会拒绝未知 key。
   * 于是升级到新清单的人会卡在「一保存就报错，但界面上找不到那个项」。
   * 界面必须把它们显示出来并提供清理入口（`pruneStaleMarks`），
   * 否则用户没有出路。
   */
  stale_keys: string[];
}

export interface RuntimeConfig {
  /**
   * **默认**模式 —— 只用来给界面上那个模式选择器**设初值**。
   *
   * ★★ 真正跑的是 `RunChoice.mode`（本次任务的运行参数）。命令层**不读**这个字段。
   * 两者可以不一致（界面上切了模式、没点保存），而不一致时以界面为准。
   */
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
   * 默认 `(0.62, 0.5)` = 上下居中、左右偏右一点（由后端常量下发，前端不另写一份）。
   * 不用区域中心的原因：联系人候选区的左边界是 0.14，**正好落在姓名那一列上**，
   * 区域中心会压住姓名与消息预览的交界处；而左边界取 0 会把导航图标栏与
   * 头像列（含未读红点）一起圈进来，红点会被 OCR 并进姓名里（实测把「李四」
   * 读成「0 李四」），逐字匹配就永远找不到人。
   */
  scroll_anchor: { x: number; y: number };
  /**
   * 标定时记录的窗口几何。
   *
   * 客户端由操作者手动启动并登录，任务只在**这个尺寸**下工作：尺寸对不上会先
   * **自动把窗口调回这个尺寸**，客户端的最小尺寸不允许时才会转人工——绝不按错的
   * 尺寸去点。真实模式下这一项必须有值，否则任务会被拒绝装配。
   */
  calibrated_window: WindowGeometry | null;
  /**
   * 按显示器缩放保存的多份界面标定。任务装配时按当前缩放挑选。
   * 旧配置没有这个字段时后端会迁成一份。
   */
  calibrations?: CalibrationSnapshot[];
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
  /**
   * 是否在查找联系人之前，用**模板匹配**找到左侧导航图标并点它一下，
   * 把视图切到「能查到联系人」的那个页面。
   *
   * 图标上没有文字，OCR 读不到它，所以这一步只能靠模板匹配。
   * 打开后状态轨迹里会多出一个 `navigating_to_view`。
   *
   * **打开时必须配模板**（`nav_icon_templates` 非空），否则任务在装配期就被拒绝。
   */
  navigate_before_search: boolean;
  /**
   * 「用于联系人导航」勾选的图标名（通讯录 / 联系人）。搜索式会先点它。
   * 一个名字底下可以有多张图（选中 / 未选中 / 带气泡），它们**全都**会参与匹配。
   */
  nav_icon_templates: string[];
  /**
   * 「用于对话历史导航」勾选的图标名（聊天 / 微信）。列表扫描式会先点它回到会话列表。
   * 老配置没有此键时按空数组处理。
   */
  chat_history_nav_templates?: string[];
  /**
   * 界面标定拖出来的框，key 见后端 `calibration::ITEMS`。
   *
   * 为什么是可选：**老配置文件里没有这个键**。后端那个字段带
   * `#[serde(default)]`，文件里没有就不会下发，前端读到 `undefined`。
   * 读的地方一律 `config.area_marks ?? {}`——**不要补默认框**，
   * 猜出来的框会让人以为已经标好了，然后对着它调半天。
   */
  area_marks?: Record<string, AreaMark>;
  /**
   * 图标库目录。留空 = 用默认（数据目录下的 `icons/`）。
   *
   * 默认放在数据目录里而不是 AppData：AppData 底下那层目录名是包标识符，
   * 没人记得住；而图标模板是人对着屏幕一张一张框出来的素材。
   * 写相对路径时挂在**程序运行当前路径**上（与数据目录同一条规则）。
   */
  icons_dir: string | null;
  /** 图标匹配的最低分数（0–1），低于它转人工。 */
  nav_icon_min_score: number;
  /** 导航图标搜索区，相对窗口比例 `[x, y, w, h]`。 */
  nav_strip: [number, number, number, number];
  /**
   * 本次任务走哪条路。见后端 `automation_core::Workflow`。
   *
   * 「找联系人」有两条**看不同界面**的路，而它们的失败现象一模一样
   * （都是"找不到联系人"），所以必须由操作者显式选，不能自动判断：
   *
   * - `search_contact`：点顶部搜索框 → 逐字输入 → 从联想下拉里挑人 →
   *   资料页 → 点「发消息」→ 输入正文。需要 `main_search` /
   *   `search_dropdown` / `contact_profile` 三块区域。
   * - `scroll_list_contact`：在左侧会话列表里滚动扫描找人。只用「列表区」。
   * - `navigate_only`：**只做导航**——找到图标、点它、结束。不找任何人，
   *   用来单独验证图标匹配准不准。
   *
   * ★★ **真正跑的那条路不在这里。** 它是本次任务的运行参数（见 `RunChoice`
   * 与 `StartTaskRequest.run_choice`）：界面选完就随任务请求发下去，
   * **不写进配置文件**。这个字段只决定界面打开时默认选中哪一项。
   *
   * 之所以拆开：放在配置里就有两处真相——界面改的是草稿、命令层读的是已保存的
   * 那一份，于是表现为「界面上选了 A、跑的是 B」。
   */
  workflow: Workflow;
  /**
   * `navigate_only` 要点哪一个图标。其余工作流不读它。**同上：只是初始值。**
   *
   * 值是**图标库里的名字**（`data/icons/` 下的一级目录名），与 `RunChoice.nav_target`
   * 同一个值域。空串 = 还没选过。
   */
  nav_target: string;
  /**
   * 位置先验的分数容差，`0` = **关掉**先验。
   *
   * 导航栏是一列纵向排列、彼此长得很像的图标，逐张模板取最高分时偶尔会出现
   * 「旁边那个图标分数略高一点」。而这件事有先验可用：**越靠近导航区中心的
   * 命中越可信**。容差决定"分数差多少以内才允许用位置来取舍"——
   * 定大了等于用位置替代了识别，定小了先验基本不生效。
   */
  icon_prior_score_tolerance: number;
  /**
   * 逐字输入时**字符之间**的间隔（毫秒）。`0` = 不留间隔。
   *
   * 搜索框是联想式的：一次性灌进去的字符会让联想请求互相打断，
   * 下拉列表只按第一个字符的结果定格——现象是"搜出来的东西不对"，
   * 不会让人想到是**输入太快**。做成配置是因为不同机器处理输入的速度差很多。
   */
  typing_interval_ms: number;
  /**
   * 资料页里"进入聊天"那个入口上的文字（默认「发消息」）。
   *
   * 靶标相关的文字，换客户端版本就可能不一样。**不能留空**：
   * 空串在「包含」判断里会匹配到任何一行，结果不是"找不到"而是**找错**。
   */
  profile_chat_entry_text: string;
  /**
   * 搜索下拉里可作为「联系人」的分组标题（默认「联系人 / 最常使用」）。
   *
   * 可写多项，用 `/`、`、` 或空白分隔。编排时在这些分组标题与下一组
   * （群聊 / 聊天记录 / 公众号 / 小程序）之间找人。同样不能留空。
   */
  search_contact_group_label: string;
  /**
   * 聊天页输入框旁边那个发送按钮上的文字（默认「发送」）。
   *
   * 发送靠**点这个按钮**，不靠快捷键（发送键设置因人而异，按错只是多个换行）。
   * 它要在「界面标定」的「发送按钮」那一块内被认出来。同样不能留空。
   */
  send_button_text: string;
}

/** 工作流。见 `RunChoice.workflow`。 */
export type Workflow = "navigate_only" | "search_contact" | "scroll_list_contact";

/**
 * 工作流的下拉选项。
 *
 * 文案要说清**看的是哪个界面**，而不只是"查找联系人"：
 * 两条路的失败现象一模一样，说清区别才能让人选对、也才能在失败时
 * 一眼看出是哪条路出的问题。
 */
export const WORKFLOW_LABELS: Record<Workflow, string> = {
  search_contact: "搜索式查找联系人（点顶部搜索框 → 从下拉里挑人 → 资料页 → 发消息）",
  scroll_list_contact: "列表扫描式查找联系人（在左侧会话列表里滚动找人）",
  navigate_only: "只做导航（找到图标并点击，不查找任何人）",
};

/** 一项标定区域对某条工作流的必要性（后端算好下发）。 */
export interface MarkRequirement {
  /** 标定项的 key（`calibration::ITEMS` 里的那个）。 */
  key: string;
  /** 界面上的说法（标定清单里的 `label`）。 */
  label: string;
  /** **这份配置里**标了没有。 */
  marked: boolean;
}

/**
 * 一条工作流要用到哪些标定区域。
 *
 * ⚠️ 这份清单由**后端**下发（`runtime::workflow_requirements`），
 * 前端**不要**自己写一份：它同时就是装配期拒绝任务的那条判据，
 * 两边不一致时的表现是「界面说齐了、点开始却被拒」。
 */
export interface WorkflowRequirement {
  workflow: Workflow;
  /** 工作流的名字，取自后端 `Workflow::describe`。 */
  label: string;
  /** 必须标好的区域；空表 = 这条工作流一块新增区域都不需要。 */
  required: MarkRequirement[];
  /**
   * 这条工作流要不要填「外部联系人名称」。
   *
   * ★ 判据在后端（`runtime::workflow_inputs`），界面只渲染：`false` 时那个框
   * **整个不显示**，「开始任务」也不拿它当门槛。前端自己判断的话，
   * 两边不一致的表现是「按钮点不动、也不说为什么」——2026-09-20 实测过。
   */
  needs_contact: boolean;
  /** 这条工作流要不要填「消息正文」。同上。 */
  needs_message: boolean;
}

/**
 * 一个「一键截屏」热键组合。
 *
 * 字段与后端 `capture_hotkey::HotkeyRequest` 一一对应，serde 直接按名读，没有命名转换。
 *
 * ⚠️ **必须至少勾一个修饰键**。只有主键的组合会被后端拒绝——注册一个"全局 A"
 * 会让用户在任何程序里都打不出 a，那不是"配置没生效"，是把别人的键盘弄坏。
 */
export interface HotkeyRequest {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  win: boolean;
  /**
   * 主键。只能是 `A`–`Z`、`0`–`9` 或 `F1`–`F12`。
   *
   * 用**字符串**而不是数字：`F1`–`F12` 没法用一个数字表达，而混着两种类型
   * 会让界面上的下拉框取值变得别扭。后端解析规则见
   * `platform_windows::hotkey::HotkeyKey::parse`。
   */
  key: string;
}

/** 图标库里的一张图：同一个图标的某一种样子。 */
export interface IconVariant {
  /** PNG 的完整路径。 */
  file: string;
  /**
   * 相对**图标库目录**的路径（`聊天/2.png`）。
   *
   * 删除单张图时用它指名道姓——不传绝对路径，是因为那是个删除命令，
   * 后端只接受"图标库目录下、且属于这个名字"的路径。
   */
  relative: string;
  /** 模板尺寸（窗口像素）。读不出来时是 0。 */
  width: number;
  height: number;
  bytes: number;
  /** `data:image/png;base64,...`；文件读不出来时是空串。 */
  image: string;
  /**
   * 这张图**现在不能当模板用**的原因（尺寸越界、文件损坏）。
   *
   * 不为 `null` 时列表里要显眼地标出来：它会让任务在装配期直接失败，
   * 提前看到总比那时候才发现好。
   */
  problem: string | null;
}

/**
 * 图标库里的一项：**一个名字 + 它的全部变体**。
 *
 * 同一个图标在选中 / 未选中 / 带气泡提醒 / 气泡里数字不一样时长得都不一样，
 * 而它们指的是同一个图标——所以配置引用的是名字，一个名字底下有几张图就存几张。
 */
export interface IconEntry {
  /** 名字。配置里引用的就是它。 */
  name: string;
  /** 这个名字对应的目录（旧式的单文件图标则是那个文件本身）。 */
  path: string;
  /** 这个名字下的全部图，顺序即变体编号。 */
  variants: IconVariant[];
  /**
   * 能不能拿去当导航模板：**每一张**都要读得出来。
   *
   * 由后端算好下发，界面不拿 `variants` 再判一遍——判据写在两处，
   * 迟早会出现「界面说能用、任务装配时却被拒」。
   */
  usable: boolean;
}

/** 「定位并点击」的结果。 */
export interface IconClickResult {
  window: Rect;
  /** 预览图的像素尺寸（点击之后重截的那一张）。 */
  width: number;
  height: number;
  image: string;
  /** 搜索区，窗口内相对坐标。 */
  strip: Rect;
  hit: NavIconHit;
  /** 实际点击的**屏幕**坐标。 */
  clicked: { x: number; y: number };
  /**
   * 点击后联系人候选区的画面有没有变化。
   *
   * `false` 有两种可能，界面上必须两种都说：界面本来就停在这个视图上（正常），
   * 或者这次点击真的没生效。
   */
  changed: boolean;
  notice: string;
}

/** 一次图标命中的位置与分数（坐标均为**窗口内相对坐标**）。 */
export interface NavIconHit {
  x: number;
  y: number;
  width: number;
  height: number;
  /** 归一化互相关系数，1.0 表示完全一致。 */
  score: number;
  /** 命中的是哪一张图（相对图标库目录的路径，如 `聊天/2.png`）。 */
  template: string;
  /** 是否达到了最低分数。没过阈值也会给出位置——那才是判断模板对不对的依据。 */
  accepted: boolean;
}

/** 一张模板在当前画面上的最高分。 */
export interface NavIconScore {
  template: string;
  score: number;
  accepted: boolean;
}

/** 「测试图标匹配」的结果。 */
export interface NavIconProbe {
  window: Rect;
  /** 预览图的像素尺寸（窗口过宽时会等比缩小）。 */
  width: number;
  height: number;
  /** `data:image/png;base64,...`，可直接作为 `<img src>`。 */
  image: string;
  /** 搜索区，窗口内相对坐标。 */
  strip: Rect;
  /** 最佳命中；`null` 表示所有模板都放不进搜索区。 */
  hit: NavIconHit | null;
  /** 每张模板各自的最高分（降序）。 */
  scores: NavIconScore[];
  /** 面向操作者的一句话结论（含分数与是否过阈值）。 */
  notice: string;
}

/**
 * 「画圆」自检的结果。
 *
 * 坐标都是**屏幕**坐标（不是窗口内相对坐标）—— 这条功能测的是光标本身，
 * 与目标客户端窗口无关。
 */
export interface CircleTraceView {
  /** 圆心，就是点按钮那一刻（更准确说：倒计时结束那一刻）的光标位置。 */
  center: [number, number];
  radius: number;
  /**
   * 半径是拿**这个**窗口尺寸的短边一半算出来的。
   *
   * 回给界面是为了让人能核对"半径确实是半个窗口"——顺带也解释了
   * 「换个窗口大小，圆就跟着变」这件事。
   */
  window_width: number;
  window_height: number;
  /** 圆周被切成了多少步（步数太少时肉眼能看出多边形）。 */
  steps: number;
  /** 实际走完一圈用掉的时长（毫秒）。 */
  duration_ms: number;
  /**
   * 用的是哪个速度（像素/秒）。
   *
   * 界面上没有这个字段可配 —— 值只在后端有一处定义（平台层
   * `WindowsDesktopConfig` 的默认值，也正是任务里用的那个）。
   * 回给界面是为了能**核对**，而不是靠猜。
   */
  speed_px_per_sec: number;
  /**
   * 走完之后**实测**的光标位置（屏幕坐标）。
   *
   * ★ 这条自检画完**不回起点**：走满一圈之后会再多走一小段，停在圆上的另一个
   * 位置（与起点差 90°）。停在起点上的话，"走过一整圈"和"它根本没动"
   * 在事后看光标位置时是分不出来的。
   */
  end: [number, number];
  /**
   * 实测终点到**圆心**的距离（像素）。
   *
   * 走对了的话应当 ≈ `radius`。界面把这两个数并排显示，由人一眼对比——
   * 前端刻意**不**拿它去判"算不算动过"：那需要一个容差阈值，而阈值定在哪里
   * 都是拍脑袋，且没有任何自动判据依赖它。
   */
  end_distance_px: number;
}

/** 按显示器缩放保存的一份完整界面标定。 */
export interface CalibrationSnapshot {
  scale_factor: number;
  /** 可选短标签；空串 = 界面上不显示。 */
  label: string;
  window: WindowGeometry;
  regions: RegionConfig;
  area_marks?: Record<string, AreaMark>;
  nav_strip: [number, number, number, number];
  scroll_anchor: { x: number; y: number };
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
  /**
   * 数据目录的**实际位置**：程序运行当前路径下的 `data/`。
   *
   * 界面上要显示它。相对路径的唯一代价就是"到底落在哪儿"要看运行方式，
   * 而这个目录里装的是配置、图标模板、任务日志与证据图——
   * 路径不对时用户必须一眼能看见，而不是等到「配置怎么没生效」再来查。
   */
  data_dir: string;
  /**
   * 图标库目录（**当前生效**的那个）。模板是**文件**，用户有权知道它们存在哪。
   */
  icons_dir: string;
  /**
   * 图标库目录**留空时的默认值**（数据目录下的 `icons/`）。
   *
   * 由后端下发、界面只拿它当占位提示：前端自己拼一份的话，两边迟早不一致，
   * 而"默认到底存哪儿"恰恰最需要说准。
   */
  icons_dir_default: string;
  /** 图标模板的边长下限 / 上限（像素），由后端下发，前端不另写一份。 */
  template_min_side: number;
  template_max_side: number;
  audit_entry_count: number;
  is_windows: boolean;
  /** 当前是否运行在 macOS 上。 */
  is_macos: boolean;
  /** 真实模式是否可用（Windows 或 macOS）。 */
  live_supported: boolean;
  /**
   * 演练 / 真实两种模式**各自**那句给操作者看的提示。
   *
   * ★ 为什么是一对、而不是"当前模式那一句"：模式现在是**运行参数**，
   * 界面上选的与配置里存的可以不是同一个。只发一句就必须在两边里选一个，
   * 而选错的方向恰好是最危险的那个——提示说"演练"、实际在动真窗口。
   * 所以按**界面上当前选的那个模式**取：`info.mode_notices[activeMode]`。
   */
  mode_notices: Record<RuntimeMode, string>;
  /**
   * 启动时那次一次性数据搬迁的结果，**只在真发生过、或搬失败时**才有值。
   *
   * 数据目录从 `%APPDATA%\com.example.wecom-local-rpa\` 换到了这里，
   * 旧配置与图标库会被搬一次。不说的话：搬成功了用户会疑惑
   * 「配置怎么突然有值了」，搬失败了会以为「数据丢了」——两种都得有答案。
   */
  migration_note: string | null;
}

/** 屏幕坐标系下的矩形（像素）。 */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * 过程诊断事件流里的坐标。
 *
 * **本帧图像坐标**（原点即那一块区域的左上角），与图上画的框一致——
 * 不是屏幕坐标（那张图本身已经是区域内的画面了）。
 */
export interface ReplayRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** 事件流里的一块识别文字。 */
export interface ReplayBox extends ReplayRect {
  /** 识别到的文字，**原样**（前后空格、换行都留着——那本身是线索）。 */
  text: string;
  confidence: number;
}

/** 一个候选在这一次判定里的遭遇（事件流 `decision` 那一条的 `candidates`）。 */
export interface ReplayCandidate extends ReplayBox {
  /** 过没过它该过的那一道判据。 */
  passed: boolean;
  /** 通过 / 淘汰的具体原因，带数字（阈值、置信度、它上面那一行是什么）。 */
  reason: string;
}

/**
 * 「这一步看到了什么」。
 *
 * 与后端 `task_diagnostics/events.rs` 里 `kind == "read"` 那条一一对应。
 */
export interface ReplayReadEvent {
  kind: "read";
  /** 记下它的时刻（本地时间，人读的）。 */
  t: string;
  /** 步骤名，与 `decision` 那一条对得上。 */
  step: string;
  /** 第几步（从 1 起，与 `steps/` 里文件名前缀一致）。 */
  index: number;
  /** 标注图的**相对**路径（盘上存的就是这个，整个目录可以搬走）。 */
  image: string;
  /**
   * 同上的**绝对**路径，由后端拼好。
   *
   * 界面读图走 asset 协议，协议按绝对路径放行；相对路径交给它取不到图。
   * 分隔符也归后端管——前端不拼路径。
   */
  image_path: string;
  /** 这一步要看的那块区域（屏幕坐标）。 */
  region: { x: number; y: number; w: number; h: number };
  frame: { w: number; h: number; fingerprint: string };
  boxes: ReplayBox[];
}

/** 重跑这条判据要什么输入。`null`（缺这个键）= 这条判据不支持离线重跑。 */
export type ReplayInput =
  | { kind: "dropdown"; keyword: string; group_labels: string }
  | { kind: "name_match"; expected_name: string; relaxed: boolean };

/** 「这一步怎么判的」。与后端 `kind == "decision"` 那条一一对应。 */
export interface ReplayDecisionEvent {
  kind: "decision";
  t: string;
  step: string;
  /** 这一步在问什么，一句人话。 */
  question: string;
  /** 按什么判的（阈值、分组、截断规则…）。 */
  rule: string;
  /** 结论：选中了什么，或者为什么停下来。 */
  outcome: string;
  /**
   * 这条判据通过了没有。
   *
   * ★ 界面「默认落在失败那一步」靠的就是它。不从 `outcome` 那句话里猜——
   * 措辞会改，而这是个结构化的事实。
   */
  passed: boolean;
  min_confidence: number;
  replay: ReplayInput | null;
  candidates: ReplayCandidate[];
}

/**
 * 事件流里的一条。
 *
 * 只列已知的两种 `kind`。读到别的（将来加了新事件）时，配对逻辑会**跳过**它，
 * 而不是当成坏数据显示——判据在 `replayView.ts` 一处。
 */
export type ReplayEvent = ReplayReadEvent | ReplayDecisionEvent;

/** `read_task_events` 的返回值。 */
export interface TaskReplayData {
  /**
   * 任务目录的绝对路径。
   *
   * `null` = 这次任务没有过程诊断材料（旧布局的任务只有一个日志文件）。
   * 界面据此说一句实话，而不是显示一个空面板。
   */
  dir: string | null;
  events: ReplayEvent[];
}

/** 「过程重放」里的一步：先看图，再看它怎么判的。 */
export interface ReplayStep {
  /** 第几步（取自 `read.index`；没有图的那一步取序号）。 */
  index: number;
  /** 步骤名（`read.step` 与 `decision.step` 是同一套措辞）。 */
  label: string;
  /** 记下它的时刻，用图那一笔的（两者本来就是同一刻）。 */
  at: string;
  read: ReplayReadEvent | null;
  decision: ReplayDecisionEvent | null;
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

/**
 * 本次任务走哪一条路 —— **运行参数，不进配置文件**。
 *
 * 界面上的选择是**即时生效**的：选完直接点「开始任务」，不需要先点「保存配置」。
 * 后端 `start_task` 只认这份参数，既不去读配置里那几个同名字段，也不把它们写回去。
 *
 * 为什么不做成配置：配置回答的是"这台机器怎么配"，这份回答的是"这一次要做什么"。
 * 混在一起必然出现两处真相（界面改的是草稿、命令层读的是已保存的那份），
 * 2026-09-19 实测的表现就是「界面上选了别的、跑的还是搜索式」。
 */
export interface RunChoice {
  /**
   * 这一次跑**演练还是真实**。
   *
   * ★ 它和 `RuntimeConfig.mode` 是同一个值域，但回答的是不同的问题：
   * 配置里那个是"这台机器默认怎么跑"（只用来给界面**设初值**），
   * 这里是"这一次怎么跑"。
   *
   * 它不只是"换一组端口"——还决定要不要带标定窗口（演练模式没有"真实窗口
   * 尺寸"这回事）、以及审计里记的平台字段。所以它必须跟着请求走，
   * 否则会出现"界面切成真实、实际按演练跑"，反方向更危险。
   */
  mode: RuntimeMode;
  workflow: Workflow;
  /**
   * 「只做导航」要点哪一个图标 —— **图标库里的名字**（`data/icons/` 下的一级目录名）。
   *
   * 只有 `workflow === "navigate_only"` 会读它。空串 = 还没选，
   * 后端装配期会直接拒绝，不会替你挑一个。
   *
   * 为什么是图标库里的名字而不是写死的两个选项：图标库里通常有四五个图标
   * （发现 / 收藏夹 / 聊天历史 / 通讯录…），写死的选项**选不出来也对不上**。
   * 选中的那个目录下的**全部图**一起参与匹配、取最高分——它们本来就是
   * 同一个图标的不同状态，不该被当成不同图标比高低。
   */
  nav_target: string;
}

export interface StartTaskRequest {
  external_contact_name: string;
  text: string;
  created_by?: string | null;
  /** 本次走哪条路。**必填**——后端没有兜底，缺了会被直接拒掉。 */
  run_choice: RunChoice;
}

/**
 * 「新建任务」表单能填的那部分。
 *
 * `run_choice` 不在里面：它由 `App` 在提交时补上（界面上的选择活在 `App` 里，
 * 与表单无关）。用 `Omit` 而不是把字段重抄一遍，是为了让 `StartTaskRequest`
 * 以后加字段时**这里会编译失败**——逼着人想清楚"新字段该由表单填、还是由 App 补"。
 */
export type TaskFormValues = Omit<StartTaskRequest, "run_choice">;

