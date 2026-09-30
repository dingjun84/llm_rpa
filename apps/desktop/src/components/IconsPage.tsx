import { useState } from "react";

import type { RuntimeConfig, RuntimeInfo } from "../types";
import { IconLibraryPanel } from "./IconLibraryPanel";
import { YoloRemotePanel } from "./YoloRemotePanel";

type SubTab = "template" | "remote";

interface Props {
  info: RuntimeInfo;
  draft: RuntimeConfig;
  onPatch: (patch: Partial<RuntimeConfig>) => void;
  onSave: () => void;
  dirty: boolean;
  canSave: boolean;
  busy: boolean;
}

/**
 * 「图标库 / 图标检测」页：子页签切换模板匹配与远程 YOLO 检测。
 *
 * ★ 故意不把远程 UI 塞进 `IconLibraryPanel`（已超基线），只在这里做薄切换。
 */
export function IconsPage(props: Props) {
  const [sub, setSub] = useState<SubTab>("template");

  return (
    <>
      <div className="guide-stage-actions" role="tablist" aria-label="图标检测方式">
        <button
          type="button"
          role="tab"
          aria-selected={sub === "template"}
          className={sub === "template" ? "tab is-active" : "tab"}
          onClick={() => setSub("template")}
        >
          模板匹配
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={sub === "remote"}
          className={sub === "remote" ? "tab is-active" : "tab"}
          onClick={() => setSub("remote")}
        >
          远程检测
        </button>
      </div>

      {sub === "template" ? (
        <IconLibraryPanel {...props} />
      ) : (
        <YoloRemotePanel draft={props.draft} busy={props.busy} />
      )}
    </>
  );
}
