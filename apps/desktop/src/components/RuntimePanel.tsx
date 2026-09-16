import { useEffect, useState } from "react";

import {
  SCENARIO_LABELS,
  type DemoScenario,
  type RuntimeConfig,
  type RuntimeInfo,
  type RuntimeMode,
} from "../types";
import { RegionCalibration, regionsAreValid } from "./RegionCalibration";

interface Props {
  info: RuntimeInfo;
  onSave: (config: RuntimeConfig) => void;
  busy: boolean;
}

const SCENARIOS = Object.keys(SCENARIO_LABELS) as DemoScenario[];

export function RuntimePanel({ info, onSave, busy }: Props) {
  const [draft, setDraft] = useState<RuntimeConfig>(info.config);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    setDraft(info.config);
  }, [info.config]);

  const update = <K extends keyof RuntimeConfig>(key: K, value: RuntimeConfig[K]) => {
    setDraft((current) => ({ ...current, [key]: value }));
    setSaved(false);
  };

  // 演练模式不碰真实窗口，标定值用不上；真实模式则必须保证四个区域都合法，
  // 否则后端 `RelativeRegion::validate` 会在执行时才报错，白白浪费一次任务。
  const canSave = draft.mode === "dry_run" || regionsAreValid(draft.regions);

  return (
    <section className="panel">
      <h2>运行模式</h2>

      <p className={draft.mode === "dry_run" ? "notice" : "notice notice-live"}>
        {draft.mode === "dry_run"
          ? "演练模式：全部使用替身端口，不会启动企业微信、不会产生任何真实输入。"
          : "真实模式：会操作本机企业微信窗口。发送期间请勿切换窗口或操作鼠标键盘。"}
      </p>

      <label className="field">
        <span className="field-label">模式</span>
        <select
          value={draft.mode}
          onChange={(event) => update("mode", event.target.value as RuntimeMode)}
        >
          <option value="dry_run">演练模式</option>
          <option value="live" disabled={!info.is_windows}>
            真实模式{info.is_windows ? "" : "（仅 Windows）"}
          </option>
        </select>
      </label>

      {draft.mode === "dry_run" ? (
        <label className="field">
          <span className="field-label">演练场景</span>
          <select
            value={draft.demo_scenario}
            onChange={(event) => update("demo_scenario", event.target.value as DemoScenario)}
          >
            {SCENARIOS.map((scenario) => (
              <option key={scenario} value={scenario}>
                {SCENARIO_LABELS[scenario]}
              </option>
            ))}
          </select>
          <span className="field-hint">
            选一个失败场景，可以直观看到"不确定就不发送"的收敛结果。
          </span>
        </label>
      ) : (
        <>
          <label className="field">
            <span className="field-label">企业微信可执行文件路径</span>
            <input
              type="text"
              value={draft.wecom_exe ?? ""}
              placeholder="必须显式配置；留空则拒绝启动"
              onChange={(event) => update("wecom_exe", event.target.value || null)}
            />
          </label>

          <label className="field">
            <span className="field-label">可执行文件 SHA-256（可选）</span>
            <input
              type="text"
              value={draft.wecom_exe_sha256 ?? ""}
              placeholder="填写后，哈希不一致将拒绝启动"
              onChange={(event) => update("wecom_exe_sha256", event.target.value || null)}
            />
          </label>

          <label className="field">
            <span className="field-label">窗口类名</span>
            <input
              type="text"
              value={draft.window_class}
              onChange={(event) => update("window_class", event.target.value)}
            />
          </label>

          <label className="field">
            <span className="field-label">本地 OCR 程序路径</span>
            <input
              type="text"
              value={draft.ocr_command ?? ""}
              placeholder="留空表示未配置，任务会停在人工处理"
              onChange={(event) => update("ocr_command", event.target.value || null)}
            />
            <span className="field-hint">
              OCR 程序从标准输入读取 PNG，向标准输出写 JSON 数组；不允许联网。
              仓库里自带 <code>tools/winocr</code>（用 Windows 内置离线 OCR），
              构建后填 <code>&lt;仓库&gt;/target/debug/winocr.exe</code> 即可。
            </span>
          </label>

          <RegionCalibration
            regions={draft.regions}
            onChange={(regions) => update("regions", regions)}
            disabled={busy}
          />
        </>
      )}

      <label className="field">
        <span className="field-label">人工确认有效期（秒）</span>
        <input
          type="number"
          min={5}
          max={600}
          value={draft.confirmation_ttl_secs}
          onChange={(event) =>
            update("confirmation_ttl_secs", Number(event.target.value) || 60)
          }
        />
      </label>

      <label className="field">
        <span className="field-label">最低 OCR 置信度</span>
        <input
          type="number"
          min={0}
          max={1}
          step={0.01}
          value={draft.min_confidence}
          onChange={(event) =>
            update("min_confidence", Number(event.target.value) || 0)
          }
        />
      </label>

      <button
        className="primary"
        disabled={busy || !canSave}
        onClick={() => {
          onSave(draft);
          setSaved(true);
        }}
      >
        {!canSave ? "标定不合法，无法保存" : saved ? "已保存" : "保存配置"}
      </button>

      <dl className="meta-list">
        <dt>数据目录</dt>
        <dd className="mono">{info.data_dir}</dd>
        <dt>审计记录</dt>
        <dd>{info.audit_entry_count} 条</dd>
      </dl>
    </section>
  );
}
