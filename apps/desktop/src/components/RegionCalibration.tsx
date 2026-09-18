import { useEffect, useState } from "react";

import { previewTargetWindow } from "../api";
import type { RegionConfig, WindowPreview } from "../types";

interface Props {
  regions: RegionConfig;
  onChange: (regions: RegionConfig) => void;
  disabled: boolean;
  /**
   * 要截的窗口类名。**必须传界面上此刻显示的值（草稿）**，不要传已保存的配置：
   * 用户改了类名还没保存时两者不同，传错了就会报"找不到窗口"。
   */
  windowClass: string;
  /**
   * 目标程序的可执行文件路径（草稿值，可为空）。
   *
   * 传它是为了让标定与任务执行用**同一套定位规则**：Qt 系程序所有顶层窗口
   * 共用同一个类名，只按类名截到的可能是登录窗——那样标定就白做了。
   */
  wecomExe: string | null;
}

/**
 * 默认标定，与后端 `RegionConfig::default()` 保持一致。
 *
 * ⚠️ 后端那份现在从 `automation_core::DEFAULT_REGIONS` 派生（单一来源），
 * 而这里**只能手抄**——TypeScript 读不到 Rust 常量。改后端那四个常量时，
 * 这个对象必须一起改；两边不一致不会报错，只会让界面画的和实际裁的是两套区域。
 *
 * `contact_panel` 的左边界是 0.14 而不是 0.0：微信会话列表左侧的导航图标栏
 * 与头像列会被 OCR 按行并进联系人姓名里（实测把「丁俊」读成「0 丁俊」），
 * 而姓名匹配是**逐字精确**的，多一个字就永远匹配不上。
 * 详细实测数据见 `automation_core::DEFAULT_CONTACT_PANEL` 的文档。
 */
export const DEFAULT_REGIONS: RegionConfig = {
  contact_panel: [0.14, 0.12, 0.28, 0.88],
  chat_header: [0.28, 0.0, 0.72, 0.1],
  chat_body: [0.28, 0.1, 0.72, 0.72],
  composer: [0.28, 0.82, 0.72, 0.18],
};

interface RegionMeta {
  key: keyof RegionConfig;
  /** 编号 1~4。与 `screen_probe annotate` 画在截图上的角标一致，便于口头沟通。 */
  index: number;
  label: string;
  hint: string;
  color: string;
}

/**
 * 四个区域。
 *
 * **编号与颜色必须与 `screen_probe annotate` 完全一致**——两处都会把编号画到截图/预览上，
 * 操作者用「把 1 的左边界挪到 32%」这种话沟通，编号对不上就没法说了。
 *
 * 颜色是照"叠在截图上还能看清"选的（中等明度、不用纯色），不是 UI 主题色。
 */
const REGIONS: RegionMeta[] = [
  {
    key: "contact_panel",
    index: 1,
    label: "联系人候选区",
    hint: "OCR 要在这里找到唯一逐字匹配的姓名。找不到、或找到多个，任务都会转人工处理。左边界要让开左侧图标栏与头像列——头像上的未读红点会被 OCR 并进姓名里（实测把「丁俊」读成「0 丁俊」），而姓名是逐字精确匹配，多一个字符就永远找不到人。",
    color: "#e24b4a",
  },
  {
    key: "chat_header",
    index: 2,
    label: "聊天页标题区",
    hint: "点开联系人之后，用这里的标题做第二次姓名核验。",
    color: "#378add",
  },
  {
    key: "chat_body",
    index: 3,
    label: "聊天正文区",
    hint: "发送前后各截一次：既要看到画面变化，也要在里面识别出这条消息本身。",
    color: "#639922",
  },
  {
    key: "composer",
    index: 4,
    label: "消息输入框区",
    hint: "粘贴前会先点这里。少了这一步，文字可能粘进搜索框而不是输入框。",
    color: "#ef9f27",
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
export function RegionCalibration({
  regions,
  onChange,
  disabled,
  windowClass,
  wecomExe,
}: Props) {
  const [preview, setPreview] = useState<WindowPreview | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 换了目标窗口，旧截图就作废了——继续挂在屏幕上会让人对着 A 窗口的图
  // 调 B 窗口的比例。宁可清掉，也不要展示一张标错对象、却看不出错在哪的图。
  useEffect(() => {
    setPreview(null);
    setError(null);
  }, [windowClass, wecomExe]);

  const problems = REGIONS.map((meta) => validateRegion(regions[meta.key])).filter(
    (message): message is string => message !== null,
  );

  const update = (key: keyof RegionConfig, index: number, percent: number) => {
    const next = [...regions[key]] as [number, number, number, number];
    next[index] = Math.max(0, Math.min(1, percent / 100));
    onChange({ ...regions, [key]: next });
  };

  const classReady = windowClass.trim().length > 0;

  const capture = async () => {
    setLoading(true);
    setError(null);
    try {
      setPreview(await previewTargetWindow(windowClass, wecomExe));
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
          <button
            type="button"
            disabled={loading || disabled || !classReady}
            onClick={capture}
          >
            {loading ? "截取中…" : preview ? "重新截图并标注" : "截图并标注"}
          </button>
          <button type="button" disabled={disabled} onClick={() => onChange(DEFAULT_REGIONS)}>
            恢复默认
          </button>
        </div>
      </div>

      <p className="field-hint">
        {classReady
          ? "先打开并登录企业微信、把窗口调到实际使用的尺寸，再点「截图并标注」。"
          : "上面还没填窗口类名——先点「指认窗口」把鼠标停在目标窗口上，或手工填一个类名。"}
        截图上会叠出 10% 网格和四个区域（编号 1~4）——对着图看比例对不对，
        不对就直接改下面的百分比。比例是相对窗口的，所以之后窗口挪动不需要重新标定。
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
            {/* 网格在图片之上、区域之下：区域填充是半透明的，网格还能透出来。 */}
            <div className="calibration-grid" aria-hidden="true" />
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
                    {meta.index} {meta.label}
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
              <span className="calibration-swatch" style={{ background: meta.color }}>
                {meta.index}
              </span>
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
