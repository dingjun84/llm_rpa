interface Props {
  /** 失效的标定项 key。由后端下发（`CalibrationPlan.stale_keys`），已按字典序排好。 */
  keys: string[];
  /** 正在清理。 */
  cleaning: boolean;
  /** 有别的操作在进行——禁用按钮，免得和保存 / 截图抢同一份配置。 */
  busy: boolean;
  /** 点「清理这几项」。清理本身由调用方负责：它还要把**草稿**里的同样几个键删掉。 */
  onPrune: () => void;
}

/**
 * 失效标定的提示条。
 *
 * ## 为什么需要它
 *
 * 清单会随流程完善而改（改名、拆并、删项），而配置是**持久化**的：清单改过之后，
 * 旧 key 就留在了 `area_marks` 里。它们不会被任何流程读到，**却会挡住保存**——
 * 后端落盘前拒绝未知 key。
 *
 * 于是升级到新清单的人会卡在「一保存就报错，但界面上找不到那个项」：
 * 报错说得没错（那个键确实没人读），可**没有出口**。这条提示就是那个出口。
 *
 * 放在最上面，是因为它挡着的正是「保存配置」——放在下面的话，
 * 用户只会看到"保存失败"，然后不知道该动哪里（那几项在列表里根本不存在）。
 *
 * `keys` 为空时**整个不渲染**（返回 `null`），调用方不必自己判断。
 */
export function StaleMarksNotice({ keys, cleaning, busy, onPrune }: Props) {
  if (keys.length === 0) return null;

  return (
    <div className="guide-stale">
      <div>
        <strong>配置里有 {keys.length} 项标定已经失效</strong>
        <p className="field-hint">
          {keys.join("、")} —— 这几项在当前的标定清单里已经没有了
          （清单改过名、拆过、或者删过）。它们不会被任何流程读到，
          但<strong>会挡住保存</strong>：后端拒绝未知的项，所以现在点「保存配置」会报错，
          而这几项在下面的列表里根本找不到。清理是安全的——它们的坐标已经没有任何代码在读。
        </p>
      </div>
      <button type="button" disabled={busy || cleaning} onClick={onPrune}>
        {cleaning ? "清理中…" : "清理这几项"}
      </button>
    </div>
  );
}
