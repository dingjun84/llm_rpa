import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  ConfirmationRequest,
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
} from "./types";

export const EVENT_TASK_UPDATED = "task://updated";
export const EVENT_CONFIRMATION_REQUESTED = "task://confirmation-requested";

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
 * 在当前画面上试一次图标模板匹配，报出**分数和位置**。
 *
 * 给「先点击导航图标跳转」做标定用：模板截得对不对、阈值该定多少，
 * 都只能对着真实画面量一次。纯只读——只截屏，不点击、不聚焦、不产生任何输入。
 *
 * 参数一律取**草稿**：用户刚填好路径还没点保存时，按草稿量出来的结果
 * 才是他此刻看到的那份配置。模板的载入走的是与任务装配**同一个函数**，
 * 所以这里报出来的分数和任务里真正会用的那张图一致。
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

/** 列出图标库里的全部图标（名字、尺寸、缩略图）。纯文件读取，不碰屏幕。 */
export function listIcons(): Promise<IconEntry[]> {
  return invoke<IconEntry[]>("list_icons");
}

/**
 * 把预览图上框出来的一块存成图标模板。
 *
 * `rect` 是**预览图坐标系**里的框 `[x, y, w, h]`，`preview` 是那张预览图的像素尺寸
 * （`WindowPreview.width/height`）。两个都要传：光有框没法换算回窗口坐标。
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

/** 删除一张图标。 */
export function deleteIcon(name: string): Promise<void> {
  return invoke<void>("delete_icon", { name });
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
 * 分数不够时它会**直接报错并且不点击**：这个按钮问的是"这张模板能不能用"，
 * "分数不够但先点了"正好是最不能接受的结果。
 *
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
