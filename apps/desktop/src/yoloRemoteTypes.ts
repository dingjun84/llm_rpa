/** 与 Rust `yolo_remote` / `ghostbox_session` 镜像的传输类型。 */

export interface YoloDetectRequest {
  window_class: string;
  wecom_exe: string | null;
  api_base: string;
  conf?: number | null;
}

export interface YoloDetectionView {
  class_id: number;
  class_name: string;
  conf: number;
  /** 原图像素中心。 */
  center: [number, number];
  xyxy: [number, number, number, number];
  /** 屏幕绝对坐标（可直接 MoveMouseTo）。 */
  screen: { x: number; y: number };
}

export interface YoloDetectResult {
  window: { x: number; y: number; width: number; height: number };
  shot_width: number;
  shot_height: number;
  count: number;
  detections: YoloDetectionView[];
  annotated_image: string | null;
  notice: string;
}

export interface GhostboxMoveResult {
  x: number;
  y: number;
  code: number;
  reused_session: boolean;
  notice: string;
}

export interface GhostboxResetResult {
  detail: string;
  notice: string;
}

/** 远程检测里可定位的导航图标类别。 */
export const YOLO_MOVE_CLASSES = ["nav_chat_icon", "nav_contacts_icon"] as const;
export type YoloMoveClass = (typeof YOLO_MOVE_CLASSES)[number];
