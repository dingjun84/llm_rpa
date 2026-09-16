import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type {
  ConfirmationRequest,
  RuntimeConfig,
  RuntimeInfo,
  StartTaskRequest,
  TaskView,
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
 * 只对真实模式有效。
 */
export function previewTargetWindow(): Promise<WindowPreview> {
  return invoke<WindowPreview>("preview_target_window");
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
