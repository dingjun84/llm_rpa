import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  ConfirmationRequest,
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
