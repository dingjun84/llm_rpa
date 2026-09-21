import { useState } from "react";

import { recordWindowGeometry } from "../api";
import type { CalibrationSnapshot, PickedWindow, RuntimeConfig } from "../types";
import { WindowPicker } from "./WindowPicker";

interface Props {
  draft: RuntimeConfig;
  onPatch: (patch: Partial<RuntimeConfig>) => void;
  busy: boolean;
}

const SCALE_TOL = 0.01;

function scaleKey(scale: number): string {
  return scale.toFixed(2);
}

function sameScale(a: number, b: number): boolean {
  return Math.abs(a - b) <= SCALE_TOL;
}

function listProfiles(draft: RuntimeConfig): CalibrationSnapshot[] {
  return [...(draft.calibrations ?? [])].sort((a, b) => a.scale_factor - b.scale_factor);
}

/**
 * 「界面标定」页顶部：按显示器缩放管理多份标定。
 *
 * 顶层的窗口尺寸 / 区域 / 导航搜索区 / 滚动落点是正在编辑的工作副本；
 * 保存配置时后端会按缩放 upsert 进 `calibrations`。开跑时再按当前缩放挑选。
 */
export function CalibrationProfiles({ draft, onPatch, busy }: Props) {
  const [measuring, setMeasuring] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);

  const profiles = listProfiles(draft);
  const activeScale = draft.calibrated_window?.scale_factor ?? null;

  const applyProfile = (snap: CalibrationSnapshot) => {
    onPatch({
      calibrated_window: snap.window,
      regions: snap.regions,
      area_marks: snap.area_marks ?? {},
      nav_strip: snap.nav_strip,
      scroll_anchor: snap.scroll_anchor,
    });
    setNotice(
      `已切到缩放 ${scaleKey(snap.scale_factor)} 的标定（${snap.window.width}×${snap.window.height}）。` +
        `改完区域后记得「保存配置」。`,
    );
  };

  const measureWindow = async () => {
    setMeasuring(true);
    setNotice(null);
    try {
      const geometry = await recordWindowGeometry(draft.window_class, draft.wecom_exe);
      const existing = profiles.find((p) => sameScale(p.scale_factor, geometry.scale_factor));
      const nextProfiles = profiles.filter((p) => !sameScale(p.scale_factor, geometry.scale_factor));
      const snap: CalibrationSnapshot = {
        scale_factor: geometry.scale_factor,
        label: existing?.label ?? "",
        window: geometry,
        regions: draft.regions,
        area_marks: draft.area_marks ?? {},
        nav_strip: draft.nav_strip,
        scroll_anchor: draft.scroll_anchor,
      };
      nextProfiles.push(snap);
      nextProfiles.sort((a, b) => a.scale_factor - b.scale_factor);
      onPatch({
        calibrated_window: geometry,
        calibrations: nextProfiles,
      });
      setNotice(
        `已记录缩放 ${scaleKey(geometry.scale_factor)}：${geometry.width}×${geometry.height}` +
          ` @(${geometry.x}, ${geometry.y})。` +
          (existing ? "已覆盖同缩放的旧份。" : "这是新的一份。") +
          "接着框区域，最后点「保存配置」。",
      );
    } catch (err) {
      setNotice(`记录失败：${String(err)}`);
    } finally {
      setMeasuring(false);
    }
  };

  const removeActive = () => {
    if (activeScale == null) return;
    const next = profiles.filter((p) => !sameScale(p.scale_factor, activeScale));
    onPatch({
      calibrations: next,
      calibrated_window: null,
    });
    setNotice(`已删掉缩放 ${scaleKey(activeScale)} 的标定（还要点保存才会落盘）。`);
  };

  const ratioFromInput = (raw: string, fallback: number) => {
    if (raw.trim() === "") return fallback;
    const value = Number(raw);
    if (!Number.isFinite(value)) return fallback;
    return Math.min(1, Math.max(0, value));
  };

  return (
    <section className="calibration" style={{ marginBottom: "1rem" }}>
      <div className="calibration-head">
        <h3>指认窗口与多份标定</h3>
      </div>
      <p className="field-hint">
        内建 Retina（常见 2.0）和外接屏（常见 1.0）的布局不一样，要各标一份。
        任务开跑时按<strong>当前</strong>缩放自动挑选；没有对应份会直接拒绝，不会拿错份去点。
      </p>

      <WindowPicker
        disabled={busy}
        onApply={(picked: PickedWindow) => {
          onPatch({
            window_class: picked.class_name,
            wecom_exe: picked.exe_path ?? draft.wecom_exe,
          });
          setNotice(
            `已指认「${picked.class_name}」${picked.title ? `（${picked.title}）` : ""}。` +
              `不要求目标窗先获焦；接着可「记录窗口尺寸」。`,
          );
        }}
      />

      <label className="field">
        <span className="field-label">窗口类名 / 应用名</span>
        <input
          type="text"
          value={draft.window_class}
          onChange={(event) => onPatch({ window_class: event.target.value })}
        />
        <span className="field-hint">
          定位目标窗按它匹配。Mac 上是应用显示名；指认成功会自动填入。
        </span>
      </label>

      <div className="field">
        <span className="field-label">已保存的缩放</span>
        {profiles.length === 0 ? (
          <span className="field-hint">还没有。先填好窗口类名，再点下面的「记录窗口尺寸」。</span>
        ) : (
          <div className="calibration-actions" style={{ flexWrap: "wrap", gap: "0.5rem" }}>
            {profiles.map((snap) => {
              const selected =
                activeScale != null && sameScale(activeScale, snap.scale_factor);
              const label = snap.label.trim()
                ? `${scaleKey(snap.scale_factor)} · ${snap.label}`
                : `${scaleKey(snap.scale_factor)} · ${snap.window.width}×${snap.window.height}`;
              return (
                <button
                  key={scaleKey(snap.scale_factor)}
                  type="button"
                  disabled={busy}
                  className={selected ? "primary" : undefined}
                  onClick={() => applyProfile(snap)}
                >
                  {selected ? `正在编辑：${label}` : label}
                </button>
              );
            })}
          </div>
        )}
      </div>

      <div className="field">
        <span className="field-label">当前窗口</span>
        <div className="calibration-actions">
          <button
            type="button"
            disabled={busy || measuring || !draft.window_class.trim()}
            onClick={measureWindow}
          >
            {measuring ? "读取中…" : "记录窗口尺寸"}
          </button>
          <button
            type="button"
            disabled={busy || activeScale == null}
            onClick={removeActive}
          >
            删除当前这份
          </button>
        </div>
        {draft.calibrated_window ? (
          <span className="field-hint">
            工作副本：<strong>{draft.calibrated_window.width}×{draft.calibrated_window.height}</strong>
            {" "}@({draft.calibrated_window.x}, {draft.calibrated_window.y})，缩放{" "}
            {scaleKey(draft.calibrated_window.scale_factor)}。下面的区域框都写进这一份。
          </span>
        ) : (
          <span className="field-hint">
            还没选中 / 记录窗口。真实模式至少要有一份标定才能开跑。
          </span>
        )}
        {notice && <p className="notice">{notice}</p>}
      </div>

      <div className="field">
        <span className="field-label">滚动落点（相对列表区）</span>
        <div className="calibration-inputs">
          <label>
            <span>横向</span>
            <input
              type="number"
              min={0}
              max={1}
              step={0.01}
              value={draft.scroll_anchor.x}
              onChange={(event) =>
                onPatch({
                  scroll_anchor: {
                    ...draft.scroll_anchor,
                    x: ratioFromInput(event.target.value, draft.scroll_anchor.x),
                  },
                })
              }
            />
          </label>
          <label>
            <span>纵向</span>
            <input
              type="number"
              min={0}
              max={1}
              step={0.01}
              value={draft.scroll_anchor.y}
              onChange={(event) =>
                onPatch({
                  scroll_anchor: {
                    ...draft.scroll_anchor,
                    y: ratioFromInput(event.target.value, draft.scroll_anchor.y),
                  },
                })
              }
            />
          </label>
        </div>
        <span className="field-hint">
          默认 0.62 / 0.5（上下居中、左右偏右）。跟区域框一起按缩放保存；任务页不再单独调。
        </span>
      </div>
    </section>
  );
}
