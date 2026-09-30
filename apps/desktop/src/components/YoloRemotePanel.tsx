import { useMemo, useState } from "react";

import {
  ghostboxMoveTo,
  yoloDetectTargetWindow,
} from "../api";
import type { RuntimeConfig } from "../types";
import type { YoloDetectResult, YoloDetectionView } from "../yoloRemoteTypes";

const DEFAULT_API_BASE = "http://192.168.1.22:8080";

interface Props {
  draft: RuntimeConfig;
  /** 有任务在跑：不截屏、不动鼠标。 */
  busy: boolean;
}

/** 每个目标类取置信度最高的一条。 */
function bestByClass(
  detections: YoloDetectionView[],
  className: string,
): YoloDetectionView | null {
  let best: YoloDetectionView | null = null;
  for (const det of detections) {
    if (det.class_name !== className) continue;
    if (!best || det.conf > best.conf) best = det;
  }
  return best;
}

/**
 * 「远程检测」子页：调用 YOLO `/predict`，并对消息 / 通讯录导航图标做幽灵盒定位。
 */
export function YoloRemotePanel({ draft, busy }: Props) {
  const [apiBase, setApiBase] = useState(DEFAULT_API_BASE);
  const [conf, setConf] = useState("0.25");
  const [detecting, setDetecting] = useState(false);
  const [moving, setMoving] = useState(false);
  const [result, setResult] = useState<YoloDetectResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [log, setLog] = useState<string[]>([]);

  const windowClass = draft.window_class.trim();
  const windowReady = windowClass !== "";
  const disabled = busy || detecting || moving;

  const chat = useMemo(
    () => (result ? bestByClass(result.detections, "nav_chat_icon") : null),
    [result],
  );
  const contacts = useMemo(
    () => (result ? bestByClass(result.detections, "nav_contacts_icon") : null),
    [result],
  );

  const pushLog = (line: string) => {
    setLog((prev) => [...prev.slice(-40), line]);
  };

  const runDetect = async () => {
    setDetecting(true);
    setError(null);
    try {
      const confNum = Number(conf);
      const parsed = Number.isFinite(confNum) ? confNum : 0.25;
      const next = await yoloDetectTargetWindow({
        window_class: draft.window_class,
        wecom_exe: draft.wecom_exe ?? null,
        api_base: apiBase.trim() || DEFAULT_API_BASE,
        conf: parsed,
      });
      setResult(next);
      pushLog(next.notice);
    } catch (err) {
      setResult(null);
      setError(String(err));
      pushLog(`检测失败：${String(err)}`);
    } finally {
      setDetecting(false);
    }
  };

  const moveTo = async (label: string, det: YoloDetectionView) => {
    setMoving(true);
    setError(null);
    const { x, y } = det.screen;
    pushLog(
      `${label}：准备 ghostbox_move_to → 屏幕 (${x}, ${y}) · ${det.class_name} conf=${det.conf.toFixed(3)}`,
    );
    try {
      const moved = await ghostboxMoveTo(x, y);
      pushLog(
        `${label}：完成 code=${moved.code} reused_session=${moved.reused_session} · ${moved.notice}`,
      );
    } catch (err) {
      setError(String(err));
      pushLog(`${label}失败：${String(err)}`);
    } finally {
      setMoving(false);
    }
  };


  return (
    <section className="panel">
      <h2>远程检测</h2>
      <p className="muted-line">
        截取目标窗口的<strong>全分辨率</strong>画面，发给 YOLO 服务的{" "}
        <code className="mono">/predict</code>（带{" "}
        <code className="mono">annotated=1</code>
        ），用检测中心换算屏幕坐标后，经幽灵盒{" "}
        <code className="mono">MoveMouseTo</code> 移到消息 / 通讯录图标。
      </p>

      {!windowReady && (
        <p className="notice notice-warn">
          还没有目标窗口的类名。先到「任务」页的「目标窗口」里点「指认窗口」，
          或到「模板匹配」里确认窗口类名已填好。
        </p>
      )}

      <label className="field">
        <span className="field-label">API 地址</span>
        <input
          type="text"
          value={apiBase}
          disabled={disabled}
          onChange={(e) => setApiBase(e.target.value)}
          placeholder={DEFAULT_API_BASE}
        />
        <span className="field-hint">默认 {DEFAULT_API_BASE}；请求走后端，不经过浏览器 CORS。</span>
      </label>

      <label className="field">
        <span className="field-label">置信度阈值 conf</span>
        <input
          type="number"
          min={0}
          max={1}
          step={0.05}
          value={conf}
          disabled={disabled}
          onChange={(e) => setConf(e.target.value)}
        />
      </label>

      <p className="field-hint">
        当前窗口类名：{" "}
        <code className="mono">{windowClass || "（空）"}</code>
        {draft.wecom_exe ? (
          <>
            {" "}
            · 程序路径 <code className="mono">{draft.wecom_exe}</code>
          </>
        ) : null}
      </p>

      <div className="guide-stage-actions">
        <button
          type="button"
          className="primary"
          disabled={disabled || !windowReady}
          onClick={() => void runDetect()}
        >
          {detecting ? "检测中…" : "截窗并远程检测"}
        </button>
      </div>

      <p className="field-hint">
        幽灵盒重置改到「轨迹自检」页。本页「移到消息 / 通讯录图标」走同一进程级会话的
        MoveMouseTo；详细逐步日志在 %TEMP%\ghostbox-replay.log。
      </p>

      {error && (
        <p className="calibration-problem" role="alert">
          {error}
        </p>
      )}

      {result && (
        <>
          <p className="muted-line">{result.notice}</p>

          <div className="guide-stage-actions">
            <button
              type="button"
              disabled={disabled || !chat}
              onClick={() => chat && void moveTo("移到消息图标", chat)}
              title={
                chat
                  ? `屏幕 (${chat.screen.x}, ${chat.screen.y}) conf=${chat.conf.toFixed(3)}`
                  : "未检出 nav_chat_icon"
              }
            >
              {moving ? "移动中…" : "移到消息图标"}
            </button>
            <button
              type="button"
              disabled={disabled || !contacts}
              onClick={() => contacts && void moveTo("移到通讯录图标", contacts)}
              title={
                contacts
                  ? `屏幕 (${contacts.screen.x}, ${contacts.screen.y}) conf=${contacts.conf.toFixed(3)}`
                  : "未检出 nav_contacts_icon"
              }
            >
              {moving ? "移动中…" : "移到通讯录图标"}
            </button>
          </div>

          {!chat && (
            <p className="field-hint">未找到 nav_chat_icon（可下调 conf 再检一次）。</p>
          )}
          {!contacts && (
            <p className="field-hint">未找到 nav_contacts_icon（可下调 conf 再检一次）。</p>
          )}

          {result.annotated_image && (
            <div className="calibration">
              <div className="calibration-head">
                <h3>标注预览</h3>
              </div>
              <img
                src={result.annotated_image}
                alt="YOLO 标注图"
                style={{ maxWidth: "100%", borderRadius: 6 }}
              />
            </div>
          )}

          <div className="calibration">
            <div className="calibration-head">
              <h3>检测列表（{result.count}）</h3>
            </div>
            {result.detections.length === 0 ? (
              <p className="muted-line">没有检出目标。</p>
            ) : (
              <table className="meta-list" style={{ width: "100%", fontSize: 13 }}>
                <thead>
                  <tr>
                    <th align="left">类别</th>
                    <th align="right">conf</th>
                    <th align="left">中心（图）</th>
                    <th align="left">屏幕</th>
                  </tr>
                </thead>
                <tbody>
                  {result.detections.map((det, index) => {
                    const highlight =
                      det.class_name === "nav_chat_icon" ||
                      det.class_name === "nav_contacts_icon";
                    return (
                      <tr
                        key={`${det.class_name}-${index}`}
                        style={highlight ? { fontWeight: 600 } : undefined}
                      >
                        <td className="mono">{det.class_name}</td>
                        <td align="right">{det.conf.toFixed(3)}</td>
                        <td className="mono">
                          ({det.center[0].toFixed(1)}, {det.center[1].toFixed(1)})
                        </td>
                        <td className="mono">
                          ({det.screen.x}, {det.screen.y})
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            )}
          </div>
        </>
      )}

      {log.length > 0 && (
        <div className="calibration">
          <div className="calibration-head">
            <h3>状态日志</h3>
          </div>
          <pre className="mono" style={{ whiteSpace: "pre-wrap", fontSize: 12 }}>
            {log.join("\n")}
          </pre>
        </div>
      )}
    </section>
  );
}
