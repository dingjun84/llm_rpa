import { useState } from "react";

import { launchClient } from "../api";
import type { RuntimeConfig } from "../types";

interface Props {
  draft: RuntimeConfig;
  onPatch: (patch: Partial<RuntimeConfig>) => void;
  busy: boolean;
}

/**
 * 「目标窗口」这一组：可执行文件路径与哈希、启动客户端、窗口类名、
 * 本地 OCR 程序路径（窗口尺寸改在「界面标定」页按缩放分别记录）。
 *
 * ## 为什么单独成文件
 *
 * 这一组是**只读的准备工作**（指认、量尺寸、看路径都不点击、不输入、不发送），
 * 与运行模式无关，演练模式下也能随时做。「启动客户端」的进行中状态
 * 与面板上其它表单无关，单独放在这里更清晰。
 *
 * ★ 它是**配置**：改完必须点「保存配置」才生效。
 */
export function TargetWindowSection({ draft, onPatch, busy }: Props) {
  /** 「启动客户端」的结果提示（成功或失败）。 */
  const [launchNotice, setLaunchNotice] = useState<string | null>(null);
  const [launching, setLaunching] = useState(false);
  // 改草稿的字段。本地适配器——只为让下面的调用点短一点，判据不在这里。
  const update = <K extends keyof RuntimeConfig>(key: K, value: RuntimeConfig[K]) => {
    onPatch({ [key]: value } as Partial<RuntimeConfig>);
  };

  const startClient = async () => {
    setLaunching(true);
    setLaunchNotice(null);
    try {
      await launchClient(draft.wecom_exe, draft.wecom_exe_sha256);
      setLaunchNotice(
        "已发出启动请求。请确认客户端窗口已经打开并完成登录，再回来开始任务——" +
          "程序不会自己去判断登录有没有完成。",
      );
    } catch (err) {
      setLaunchNotice(`启动失败：${String(err)}`);
    } finally {
      setLaunching(false);
    }
  };

  return (
    /* 下面这一组是**配置**，刻意不放在「模式」分支里。
        指认窗口与区域标定都是只读操作（不点击、不输入、不发送），
        而且实际顺序本来就是"先把窗口和四个区域标定好，再决定用哪种模式跑"。
        把它们藏在真实模式后面，等于让人没法在跑真实模式之前做准备。 */
    <section className="calibration">
      <div className="calibration-head">
        <h3>目标窗口</h3>
      </div>
      <p className="field-hint">
        指认窗口已挪到「界面标定」页（按鼠标下最上层窗取，不必先获焦）。
        这里只保留路径、类名、启动与 OCR——改完仍要点「保存配置」。
      </p>

      <label className="field">
        <span className="field-label">目标程序可执行文件路径</span>
        <input
          type="text"
          value={draft.wecom_exe ?? ""}
          placeholder="在「界面标定」页点「指认窗口」可自动填入"
          onChange={(event) => update("wecom_exe", event.target.value || null)}
        />
        <span className="field-hint">
          两个用途：一是「启动客户端」按钮按它拉起程序；二是<strong>校验窗口归属</strong>——
          定位时会要求窗口属于这个程序。Qt 系程序（微信 4.x 就是）所有顶层窗口
          共用同一个类名，少了这条就可能选中登录窗。
        </span>
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

      <div className="field">
        <span className="field-label">客户端</span>
        <div className="calibration-actions">
          <button
            type="button"
            disabled={busy || launching || !(draft.wecom_exe ?? "").trim()}
            onClick={startClient}
          >
            {launching ? "启动中…" : "启动客户端"}
          </button>
        </div>
        <span className="field-hint">
          微信 / 企业微信<strong>由你启动并登录</strong>，任务不会自己去拉起程序：
          重复启动会弹出登录窗，而登录窗和主窗口类名相同，定位会选错；
          扫码、验证码这些事程序也插不上手。
        </span>
        {launchNotice && <p className="notice">{launchNotice}</p>}
      </div>

      <label className="field">
        <span className="field-label">窗口类名</span>
        <input
          type="text"
          value={draft.window_class}
          onChange={(event) => update("window_class", event.target.value)}
        />
        <span className="field-hint">
          定位目标窗口按类名匹配；填了上面的可执行文件路径时，会同时校验窗口属于该程序。
          在「界面标定」页点「指认窗口」会自动填入。截图用的也是这个值，
          改完不用保存就能直接切过去截图看效果。
        </span>
      </label>

      <p className="field-hint">
        窗口尺寸与区域框已挪到「界面标定」页，并按<strong>显示器缩放</strong>各存一份。
        任务开跑时按当前缩放自动挑选对应那份——缩放对不上会直接拒绝，不会拿错份去点。
      </p>

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
    </section>
  );
}
