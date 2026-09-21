import { useEffect, useRef, useState } from "react";

import { pickTargetWindow } from "../api";
import type { PickedWindow } from "../types";

interface Props {
  disabled: boolean;
  /** 指认成功后回填配置。只回填能确定读到的字段。 */
  onApply: (picked: PickedWindow) => void;
}

/** 倒计时时长（秒）。够把鼠标移到目标窗口上，又不至于等到不耐烦。 */
const COUNTDOWN_SECS = 5;
/** 倒计时期间的采样间隔（毫秒）。 */
const SAMPLE_INTERVAL_MS = 200;

function describe(picked: PickedWindow): string {
  const rect = picked.window;
  const title = picked.title.trim() || "<无标题>";
  return `${picked.class_name || "<无类名>"} · ${title} · ${rect.width}×${rect.height} @(${rect.x}, ${rect.y})`;
}

/**
 * 用鼠标「指认」目标窗口。
 *
 * 交互是**悬停**，不是点击：点按钮后本程序开始倒计时，操作者把鼠标停在目标窗口上，
 * 倒计时结束时采用最后采样到的那个窗口。
 *
 * 为什么不做成点击选中：点击会在目标程序里产生副作用——在会话列表上点一下就把会话
 * 打开了，在输入框上点一下就把光标挪走了。悬停读取没有任何副作用。
 *
 * 采集到的是**特征**（类名 + 所属程序路径）而不是窗口句柄：句柄不跨进程重启稳定，
 * 存下来下次就失效了。
 */
export function WindowPicker({ disabled, onApply }: Props) {
  const [active, setActive] = useState(false);
  const [remainingMs, setRemainingMs] = useState(0);
  const [hovered, setHovered] = useState<PickedWindow | null>(null);
  const [applied, setApplied] = useState<PickedWindow | null>(null);
  const [error, setError] = useState<string | null>(null);

  // 倒计时结束时要用到最新的回调，但它每次渲染都是新函数；
  // 放进 ref，免得它一变就把倒计时重置了。
  const onApplyRef = useRef(onApply);
  useEffect(() => {
    onApplyRef.current = onApply;
  }, [onApply]);
  const hoveredRef = useRef<PickedWindow | null>(null);

  useEffect(() => {
    if (!active) return;

    let alive = true;
    let settled = false;
    let left = COUNTDOWN_SECS * 1000;

    hoveredRef.current = null;
    setRemainingMs(left);
    setHovered(null);
    setError(null);

    const finish = () => {
      if (!alive || settled) return;
      settled = true;
      setActive(false);

      const picked = hoveredRef.current;
      if (!picked) {
        setError("倒计时结束，但没读到窗口——鼠标要停在目标窗口的可见区域上（不是桌面）。");
        return;
      }
      if (picked.is_self) {
        setError("读到的是本程序自己的窗口。请把鼠标移到目标程序上再试一次。");
        return;
      }
      if (!picked.class_name.trim()) {
        setError("这个窗口没有类名，不能作为匹配依据。");
        return;
      }
      setApplied(picked);
      onApplyRef.current(picked);
    };

    const id = window.setInterval(() => {
      left -= SAMPLE_INTERVAL_MS;
      setRemainingMs(Math.max(0, left));
      pickTargetWindow()
        .then((picked) => {
          if (!alive || settled) return;
          hoveredRef.current = picked;
          setHovered(picked);
          setError(null);
        })
        .catch((err) => {
          if (!alive || settled) return;
          hoveredRef.current = null;
          setHovered(null);
          setError(String(err));
        })
        .finally(() => {
          if (left <= 0) finish();
        });
    }, SAMPLE_INTERVAL_MS);

    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [active]);

  const seconds = Math.ceil(remainingMs / 1000);

  return (
    <div className="field">
      <span className="field-label">目标窗口</span>

      <div className="calibration-actions">
        {active ? (
          <>
            <button type="button" onClick={() => setActive(false)}>
              取消指认
            </button>
            <span className="picker-countdown" role="status">
              把鼠标移到目标窗口上… {seconds} 秒
            </span>
          </>
        ) : (
          <button type="button" disabled={disabled} onClick={() => setActive(true)}>
            指认窗口
          </button>
        )}
      </div>

      {active ? (
        <span className="field-hint" role="status">
          {hovered
            ? `当前指向：${describe(hovered)}`
            : "把鼠标停在目标窗口上——不要点击，点一下会在对方程序里产生副作用。"}
        </span>
      ) : (
        <span className="field-hint">
          点「指认窗口」，然后在 5 秒内把鼠标停在目标程序的主窗口上（不必先点它获焦；取的是鼠标下最上层的窗）。
          只读取类名与所属程序路径，不点击、不聚焦、不产生任何输入。
        </span>
      )}

      {error && <p className="notice notice-error">指认失败：{error}</p>}

      {applied && (
        <>
          <dl className="meta-list picker-result">
            <dt>类名</dt>
            <dd className="mono">{applied.class_name}</dd>
            <dt>标题</dt>
            <dd>{applied.title.trim() || "（空）"}</dd>
            <dt>所属程序</dt>
            <dd className="mono">{applied.exe_path ?? "（读取失败，未回填）"}</dd>
            <dt>窗口尺寸</dt>
            <dd>
              {applied.window.width}×{applied.window.height} @({applied.window.x},{" "}
              {applied.window.y})
            </dd>
          </dl>
          <span className="field-hint">
            已把类名（以及能读到的所属程序路径）填进下面的配置。
            确认无误后还要点最下面的「保存配置」才会真正生效。
          </span>
        </>
      )}
    </div>
  );
}
