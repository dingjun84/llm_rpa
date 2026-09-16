import { useState } from "react";

import { previewTargetWindow } from "../api";
import type { RegionConfig, WindowPreview } from "../types";

interface Props {
  regions: RegionConfig;
  onChange: (regions: RegionConfig) => void;
  disabled: boolean;
}

/** 默认标定，与后端 `RegionConfig::default()` 保持一致。 */
export const DEFAULT_REGIONS: RegionConfig = {
  contact_panel: [0.0, 0.12, 0.28, 0.88],
  chat_header: [0.28, 0.0, 0.72, 0.1],
  chat_body: [0.28, 0.1, 0.72, 0.72],
  composer: [0.28, 0.82, 0.72, 0.18],
};

interface RegionMeta {
  key: keyof RegionConfig;
  label: string;
  hint: string;
  color: string;
}

const REGIONS: RegionMeta[] = [
  {
    key: "contact_panel",
    label: "联系人候选区",
    hint: "OCR 要在这里找到唯一逐字匹配的姓名。找不到、或找到多个，任务都会转人工处理。",
    color: "#2563eb",
  },
  {
    key: "chat_header",
    label: "聊天页标题区",
    hint: "点开联系人之后，用这里的标题做第二次姓名核验。",
    color: "#7c3aed",
  },
  {
    key: "chat_body",
    label: "聊天正文区",
    hint: "发送前后各截一次：既要看到画面变化，也要在里面识别出这条消息本身。",
    color: "#059669",
  },
  {
    key: "composer",
    label: "消息输入框区",
    hint: "粘贴前会先点这里。少了这一步，文字可能粘进搜索框而不是输入框。",
    color: "#d97706",
  },
];

const toPercent = (value: number) => Math.round(value * 1000) / 10;

/**
 * 校验一条相对区域。
 *
 * 必须与后端 `RelativeRegion::validate` 完全一致：
 * 四个分量都在 0..=1、`x+w<=1`、`y+h<=1`、宽高大于 0。
 * 前端先拦一道，免得用户保存了一个后端会拒绝的配置。
 */
function validateRegion(region: [number, number, number, number]): string | null {
  const [x, y, w, h] = region;
  if ([x, y, w, h].some((n) => !Number.isFinite(n))) {
    return "四个分量都必须是数字";
  }
  if (x < 0 || y < 0 || w <= 0 || h <= 0) {
    return "宽高必须大于 0，坐标不能为负";
  }
  if (x + w > 1.000001 || y + h > 1.000001) {
    return "区域不能超出窗口边界（x+宽 ≤ 100%，y+高 ≤ 100%）";
  }
  return null;
}

/** 四个区域是否全部合法。保存前用它拦一道。 */
export function regionsAreValid(regions: RegionConfig): boolean {
  return REGIONS.every((meta) => validateRegion(regions[meta.key]) === null);
}

/**
 * 区域标定面板。
 *
 * 真实模式的关键前提是"四个局部区域都标定正确"——默认值是照企业微信界面
 * 猜的，不同版本、不同窗口尺寸、不同 DPI 下都需要重新标。
 * 这里让操作者截一张目标窗口的图，对着图调比例，而不是盲填数字。
 *
 * 预览是**只读**的：不会点击、不会粘贴、不会发送，也不会把目标窗口抢到前台。
 */
export function RegionCalibration({ regions, onChange, disabled }: Props) {
  const [preview, setPreview] = useState<WindowPreview | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const problems = REGIONS.map((meta) => validateRegion(regions[meta.key])).filter(
    (message): message is string => message !== null,
  );

  const update = (key: keyof RegionConfig, index: number, percent: number) => {
    const next = [...regions[key]] as [number, number, number, number];
    next[index] = Math.max(0, Math.min(1, percent / 100));
    onChange({ ...regions, [key]: next });
  };

  const capture = async () => {
    setLoading(true);
    setError(null);
    try {
      setPreview(await previewTargetWindow());
    } catch (err) {
      setPreview(null);
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  return (
    <section className="calibration">
      <div className="calibration-head">
        <h3>区域标定</h3>
        <div className="calibration-actions">
          <button type="button" disabled={loading || disabled} onClick={capture}>
            {loading ? "截取中…" : preview ? "重新截取" : "截取目标窗口"}
          </button>
          <button type="button" disabled={disabled} onClick={() => onChange(DEFAULT_REGIONS)}>
            恢复默认
          </button>
        </div>
      </div>

      <p className="field-hint">
        先打开并登录企业微信、把窗口调到实际使用的尺寸，再点「截取目标窗口」。
        比例是相对窗口的，所以之后窗口挪动或缩放都不需要重新标定。
      </p>

      {error && <p className="notice notice-error">截取失败：{error}</p>}

      {problems.length > 0 && (
        <p className="notice notice-error">
          标定不合法：{problems[0]}
          {problems.length > 1 ? `（另有 ${problems.length - 1} 处问题）` : ""}
        </p>
      )}

      {preview && (
        <>
          <div className="calibration-stage">
            <img src={preview.image} alt="目标窗口预览" />
            {REGIONS.map((meta) => {
              const [x, y, w, h] = regions[meta.key];
              return (
                <div
                  key={meta.key}
                  className="calibration-overlay"
                  style={{
                    left: `${x * 100}%`,
                    top: `${y * 100}%`,
                    width: `${w * 100}%`,
                    height: `${h * 100}%`,
                    borderColor: meta.color,
                    background: `${meta.color}1f`,
                  }}
                >
                  <span className="calibration-overlay-label" style={{ background: meta.color }}>
                    {meta.label}
                  </span>
                </div>
              );
            })}
          </div>
          <p className="field-hint">
            窗口 {preview.window.width}×{preview.window.height} @({preview.window.x},{" "}
            {preview.window.y})，截图指纹 <code>{preview.fingerprint.slice(0, 16)}</code>
          </p>
        </>
      )}

      {REGIONS.map((meta) => {
        const region = regions[meta.key];
        const problem = validateRegion(region);
        return (
          <div className="calibration-row" key={meta.key}>
            <div className="calibration-row-head">
              <span className="calibration-swatch" style={{ background: meta.color }} />
              <strong>{meta.label}</strong>
              {problem && <span className="calibration-problem">{problem}</span>}
            </div>
            <div className="calibration-inputs">
              {(["左", "上", "宽", "高"] as const).map((axis, index) => (
                <label key={axis}>
                  <span>{axis}%</span>
                  <input
                    type="number"
                    min={0}
                    max={100}
                    step={0.5}
                    disabled={disabled}
                    value={toPercent(region[index])}
                    onChange={(event) =>
                      update(meta.key, index, Number(event.target.value))
                    }
                  />
                </label>
              ))}
            </div>
            <span className="field-hint">{meta.hint}</span>
          </div>
        );
      })}
    </section>
  );
}
