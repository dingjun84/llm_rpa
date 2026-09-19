import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  CalibrationPlan,
  ConfirmationRequest,
  HotkeyRequest,
  IconClickResult,
  IconEntry,
  NavIconProbe,
  PickedWindow,
  RuntimeConfig,
  RuntimeInfo,
  StartTaskRequest,
  TaskView,
  WindowGeometry,
  WindowPreview,
  WorkflowRequirement,
} from "./types";

export const EVENT_TASK_UPDATED = "task://updated";
export const EVENT_CONFIRMATION_REQUESTED = "task://confirmation-requested";
/**
 * 「一键截屏」热键被按下。
 *
 * 事件载荷是空的：热键只负责说一句"可以截了"，**截哪张图由界面决定**——
 * 那取决于当前正在标定哪个界面、以及配置草稿里的窗口类名。
 */
export const EVENT_CAPTURE_HOTKEY = "calibration://capture-hotkey";

/** 发起一条发送任务，返回任务 ID。工作流在后台线程运行。 */
export function startTask(request: StartTaskRequest): Promise<string> {
  return invoke<string>("start_task", { request });
}

export function listTasks(): Promise<TaskView[]> {
  return invoke<TaskView[]>("list_tasks");
}

export function getTask(taskId: string): Promise<TaskView> {
  return invoke<TaskView>("get_task", { taskId });
}

/** 提交人工确认结果。只有确认之后工作流才会进入发送步骤。 */
export function confirmTask(
  taskId: string,
  approved: boolean,
  reason?: string,
): Promise<void> {
  return invoke<void>("confirm_task", { taskId, approved, reason: reason ?? null });
}

export function cancelTask(taskId: string): Promise<void> {
  return invoke<void>("cancel_task", { taskId });
}

export function runtimeInfo(): Promise<RuntimeInfo> {
  return invoke<RuntimeInfo>("runtime_info");
}

export function setRuntimeConfig(config: RuntimeConfig): Promise<void> {
  return invoke<void>("set_runtime_config", { config });
}

/**
 * 截一张目标窗口的预览图用于区域标定。
 *
 * 纯只读：不会点击、不会粘贴、不会发送，也不会把目标窗口带到前台。
 *
 * `windowClass` 与 `wecomExe` 必须传**界面上此刻显示的值**（也就是草稿），
 * 不要传已保存的配置：用户改了类名还没保存时，两者是不同的值，
 * 传错了就会报「找不到窗口」。
 *
 * 为什么要一起传 `wecomExe`：Qt 系程序（微信 4.x 就是）所有顶层窗口共用同一个类名，
 * 只按类名截到的可能是登录窗——那样标定就白做了。任务执行时是「类名 + 归属程序」
 * 一起匹配的，标定必须用同一套规则。
 */
export function previewTargetWindow(
  windowClass: string,
  wecomExe: string | null,
): Promise<WindowPreview> {
  return invoke<WindowPreview>("preview_target_window", { windowClass, wecomExe });
}

/**
 * 由操作者**手动**启动客户端。
 *
 * 程序不会在任务流程里自己拉起客户端：那会带来重复启动弹出登录窗、
 * 登录窗与主窗同类名（窗口定位会选错）、以及「还没登录完就被当成就绪」。
 * 登录是人的事——扫码、验证码、风控确认，程序插不上手。
 *
 * 参数同样取草稿值：路径存在性与 SHA-256 校验由后端沿用平台层那一套。
 */
export function launchClient(
  wecomExe: string | null,
  wecomExeSha256: string | null,
): Promise<void> {
  return invoke<void>("launch_client", { wecomExe, wecomExeSha256 });
}

/**
 * 记录目标窗口当前的尺寸与显示器缩放，供「按标定尺寸工作」使用。
 *
 * 纯只读：只读窗口矩形与显示器指标，不截屏、不点击、不聚焦。
 *
 * 定位规则与任务执行时**完全一致**（同样传类名 + 目标程序路径），
 * 否则「记下来的」和「任务看到的」可能不是同一个窗口。
 */
export function recordWindowGeometry(
  windowClass: string,
  wecomExe: string | null,
): Promise<WindowGeometry> {
  return invoke<WindowGeometry>("record_window_geometry", { windowClass, wecomExe });
}

/**
 * 读出**界面标定计划**：有哪些界面要标、每一项该框哪儿、当前标到哪一步了。
 *
 * 纯只读：只读配置，不碰屏幕、不截屏、不聚焦，因此**不按运行模式设限**。
 *
 * 清单由**后端**下发，前端不自己写一份：标定项的权威定义在 `calibration::ITEMS`，
 * 而且「哪一项存哪个配置字段」也在那里。前端另写一份的话，迟早会出现
 * 「界面上有这一项、任务里却读不到」——这种错位**不报任何错**。
 */
export function listCalibrationPlan(): Promise<CalibrationPlan> {
  return invoke<CalibrationPlan>("list_calibration_plan");
}

/**
 * 每条工作流需要哪些标定区域，以及**这份（草稿）配置里标了没有**。
 *
 * ## 为什么判据在后端
 *
 * 「这条工作流需要哪几块」就是装配期拒绝任务的那条判据
 * （后端 `runtime::required_marks`）。前端自己列一张表的话，
 * 两边不一致时的表现是「界面说齐了、点开始却被拒」——
 * 而人只会去怀疑标定本身，不会想到是界面上那张表过期了。
 *
 * ## 为什么传的是草稿
 *
 * 操作者刚把工作流改掉、还没点「保存配置」时，他要看的是
 * "我现在这份配置还缺什么"。读服务端那份会答非所问。
 */
export function workflowRequirements(
  config: RuntimeConfig,
): Promise<WorkflowRequirement[]> {
  return invoke<WorkflowRequirement[]>("workflow_requirements", { config });
}

/**
 * 校验一项界面标定的坐标，**不改任何状态**。
 *
 * 为什么不直接落盘：界面是**草稿式**保存的——拖框只改前端草稿，
 * 点「保存配置」才写盘。拖一下就写服务端配置的话，`dirty` 标记
 * 与「保存」按钮就都没意义了。
 *
 * 但校验不能省：`RegionCanvas` 已经把比例夹在 0–1 内、也留了最小尺寸，
 * 可那只覆盖了「不越界」，与编排层那条判据（`RelativeRegion::validate`）
 * 不是同一套。界面调这个命令问一遍，判据仍只有服务端那一处。
 */
export function validateAreaMark(
  key: string,
  rect: [number, number, number, number],
): Promise<void> {
  return invoke<void>("validate_area_mark", { key, rect });
}

/**
 * 清掉配置里那些**清单已经没有**的标定项，返回清掉的个数。
 *
 * 这个命令是「一保存就报错，但界面上找不到那个项」的唯一出口：
 * 清单改过之后（改名、拆并、删项），旧 key 还留在配置里，
 * 后端落盘前会拒绝未知 key ⇒ 保存失败，而那个项在界面上**不存在**，
 * 用户没有任何办法把它去掉。
 *
 * ⚠️ 它**直接落盘**（不像 `validateAreaMark` 那样只校验）：清掉的正是
 * 让保存失败的那些键，只改内存的话，用户下一次点「保存」仍然会被自己挡住。
 * 因此界面调完之后必须**重新读一遍配置**，不能拿旧草稿继续用。
 */
export function pruneStaleMarks(): Promise<number> {
  return invoke<number>("prune_stale_marks");
}

/**
 * 注册「一键截屏」的全局热键。
 *
 * ## 为什么需要它
 *
 * 客户端里有些画面是**失焦即收**的临时弹层（搜索下拉框、右键菜单）。操作者一旦点击
 * 本程序的按钮，前台就转到了本程序，弹层在点击的**那一刻**已经收起——等截屏真正执行时，
 * 截到的是收起后的画面。热键让"触发截屏"这个动作不必把前台让出来。
 *
 * ## 它只发事件，不截图
 *
 * 热键命中时后端只发一条 [`EVENT_CAPTURE_HOTKEY`]，实际截图仍走
 * [`previewTargetWindow`]。这样截的永远是"界面上此刻正在标定的那个界面 + 草稿里的窗口类名"，
 * 不会出现"界面上写着新类名、截出来的是旧窗口"。
 *
 * 返回**后端规范化后的组合键写法**（例如 `Ctrl+Alt+S`）。显示它而不是界面自己拼一份，
 * 是为了"显示的组合"和"实际注册的组合"不可能不一致。
 *
 * 注册失败会**抛出可读的原因**（最常见的是组合键被别的程序占用了），
 * 这时界面应当提示换一个组合，或者改用延时截图。
 */
export function registerCaptureHotkey(request: HotkeyRequest): Promise<string> {
  return invoke<string>("register_capture_hotkey", { request });
}

/**
 * 注销「一键截屏」热键。**幂等**——没注册时也返回成功。
 *
 * 界面在离开标定页、以及每次改组合键之前都会调它。
 */
export function unregisterCaptureHotkey(): Promise<void> {
  return invoke<void>("unregister_capture_hotkey");
}

export function onCaptureHotkey(handler: () => void): Promise<UnlistenFn> {
  return listen(EVENT_CAPTURE_HOTKEY, () => handler());
}

/**
 * 在当前画面上试一次图标模板匹配，报出**分数和位置**。
 *
 * 给「先点击导航图标跳转」做标定用：图标截得对不对、阈值该定多少，
 * 都只能对着真实画面量一次。纯只读——只截屏，不点击、不聚焦、不产生任何输入。
 *
 * 参数一律取**草稿**：用户刚改完还没点保存时，按草稿量出来的结果
 * 才是他此刻看到的那份配置。图标的载入走的是与任务装配**同一个函数**，
 * 所以这里报出来的分数和任务里真正会用的那些图一致。
 *
 * `templates` 传的是**图标名**（不是路径）；一个名字底下有几张图就量几张，
 * 报出来的是其中分数最高的那张。
 */
export function probeNavIcon(
  windowClass: string,
  wecomExe: string | null,
  navStrip: [number, number, number, number],
  templates: string[],
  minScore: number,
): Promise<NavIconProbe> {
  return invoke<NavIconProbe>("probe_nav_icon", {
    windowClass,
    wecomExe,
    navStrip,
    templates,
    minScore,
  });
}

/** 列出图标库里的全部图标（名字、全部变体、缩略图）。纯文件读取，不碰屏幕。 */
export function listIcons(): Promise<IconEntry[]> {
  return invoke<IconEntry[]>("list_icons");
}

/**
 * 把预览图上框出来的一块存成图标模板。
 *
 * `rect` 是**预览图坐标系**里的框 `[x, y, w, h]`，`preview` 是那张预览图的像素尺寸
 * （`WindowPreview.width/height`）。两个都要传：光有框没法换算回窗口坐标。
 *
 * **同一个名字可以反复调用**：第二次不是"重名错误"，而是给这个图标追加一张变体。
 * 同一个图标在选中 / 未选中 / 带气泡时长得不一样，它们指的是同一个图标。
 *
 * 后端**不会**从预览图上裁——预览是缩放过的，裁出来的模板边缘会带上插值杂色，
 * 拿去匹配原始分辨率的画面自然对不准。它会按框重新截一张原始分辨率的窗口画面。
 * 因此如果窗口在「预览」与「保存」之间改了尺寸，它会直接拒绝并要求重新截图。
 *
 * 纯只读：只截屏，不点击、不聚焦、不产生任何输入。
 */
export function saveIconFromCrop(
  name: string,
  windowClass: string,
  wecomExe: string | null,
  rect: [number, number, number, number],
  preview: [number, number],
): Promise<IconEntry> {
  return invoke<IconEntry>("save_icon_from_crop", {
    name,
    windowClass,
    wecomExe,
    rect,
    preview,
  });
}

/** 删除一个图标（它的**全部**变体一起走）。 */
export function deleteIcon(name: string): Promise<void> {
  return invoke<void>("delete_icon", { name });
}

/**
 * 删除一个图标里的**某一张**图，留下同一个名字下的其他张。
 *
 * `relative` 是**相对图标库目录**的路径（`IconVariant.relative`，如 `聊天/2.png`）。
 * 后端会校验它确实属于这个名字，别的地方一律拒绝——这是个删除命令，
 * 不能拿前端传来的字符串直接当路径用。
 */
export function deleteIconVariant(name: string, relative: string): Promise<void> {
  return invoke<void>("delete_icon_variant", { name, relative });
}

/**
 * 定位一个图标并**真的点它一下**。
 *
 * ⚠️ 这个命令**会产生真实的鼠标点击**——它是界面上那个「定位并点击」按钮，
 * 由人明确按下，不属于任务流程（任务走的是编排器里的 `NavigatingToView`）。
 *
 * 判据与任务里那一步**刻意不同**：任务里点击后画面没变只记警告、继续往下走
 * （"界面本来就停在这个视图上"是最常见的场景）；这里则如实回报 `changed`，
 * 因为按按钮的人要的就是"这一下到底有没有生效"。
 *
 * 分数不够时它会**直接报错并且不点击**：这个按钮问的是"这些图能不能用"，
 * "分数不够但先点了"正好是最不能接受的结果。
 *
 * `templates` 传的是**图标名**；一个名字底下有几张图就都参与匹配。
 * `contactPanel` 是拿来做「画面变了没有」的参照物的区域；
 * `settleMs` 复用「滚动停稳等待」那个值——等待的是同一个物理现象（界面动画）。
 */
export function clickIcon(
  windowClass: string,
  wecomExe: string | null,
  navStrip: [number, number, number, number],
  contactPanel: [number, number, number, number],
  templates: string[],
  minScore: number,
  settleMs: number,
): Promise<IconClickResult> {
  return invoke<IconClickResult>("click_icon", {
    windowClass,
    wecomExe,
    navStrip,
    contactPanel,
    templates,
    minScore,
    settleMs,
  });
}

/**
 * 读出**光标下**的窗口。
 *
 * 纯只读：不点击、不聚焦、不产生任何输入。界面在倒计时期间反复调用它做实时回显，
 * 因此它必须足够轻——不截屏、不编码。
 *
 * 为什么是「悬停」而不是「点击选中」：点击会在目标程序里产生副作用
 * （在会话列表上点一下就把会话打开了），悬停则完全没有。
 */
export function pickTargetWindow(): Promise<PickedWindow> {
  return invoke<PickedWindow>("pick_target_window");
}

export function onTaskUpdated(handler: (task: TaskView) => void): Promise<UnlistenFn> {
  return listen<TaskView>(EVENT_TASK_UPDATED, (event) => handler(event.payload));
}

export function onConfirmationRequested(
  handler: (request: ConfirmationRequest) => void,
): Promise<UnlistenFn> {
  return listen<ConfirmationRequest>(EVENT_CONFIRMATION_REQUESTED, (event) =>
    handler(event.payload),
  );
}
