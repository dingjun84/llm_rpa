import type { RuntimeConfig } from "../types";

interface Props {
  draft: RuntimeConfig;
  onPatch: (patch: Partial<RuntimeConfig>) => void;
}

/**
 * 比例输入（0–1）的取值。
 *
 * 输入框清空或内容不是数字时**保留原值**，不要退回 0：
 * 退回 0 会把「正在删掉重输」这个中间状态变成「落点跑到最左边」，
 * 而 0 恰好是个合法值，看起来就像配置已经生效了。
 */
const ratioFromInput = (raw: string, fallback: number) => {
  if (raw.trim() === "") return fallback;
  const value = Number(raw);
  if (!Number.isFinite(value)) return fallback;
  return Math.min(1, Math.max(0, value));
};

/**
 * 「超时」与「滚动查找联系人」两组参数。
 *
 * ## 为什么单独成文件
 *
 * 这两组加起来有近十个数字输入框，是全页面最长的一段；而它们的**共同点**是
 * 「旋钮多、平时不动」——排在一起才看得出来它们属于同一类：机器慢 / 窗口大 /
 * 列表长的时候才需要来调。放在主文件里时，保存按钮被顶到很远的地方，
 * 每次改配置都得滚过它们。
 *
 * ★ 全是**配置**，改完要点「保存配置」。
 */
export function AdvancedParamsSection({ draft, onPatch }: Props) {
  // 改草稿的字段。本地适配器——只为让下面的调用点短一点，判据不在这里。
  const update = <K extends keyof RuntimeConfig>(key: K, value: RuntimeConfig[K]) => {
    onPatch({ [key]: value } as Partial<RuntimeConfig>);
  };

  return (
    <>
      <div className="field">
        <span className="field-label">超时（毫秒 / 秒）</span>
        <div className="calibration-inputs">
          <label>
            <span>单次 OCR 上限</span>
            <input
              type="number"
              min={500}
              step={500}
              value={draft.ocr_timeout_ms}
              onChange={(event) =>
                update("ocr_timeout_ms", Math.max(500, Number(event.target.value) || 500))
              }
            />
          </label>
          <label>
            <span>单步下限</span>
            <input
              type="number"
              min={1}
              max={600}
              value={draft.step_timeout_secs}
              onChange={(event) =>
                update("step_timeout_secs", Math.max(1, Number(event.target.value) || 1))
              }
            />
          </label>
        </div>
        <span className="field-hint">
          机器慢或窗口大时把「单次 OCR 上限」调大即可——单步超时会自动跟着放宽
          （取<code>单步下限</code>与<code>OCR 上限 + 5 秒</code>中的较大者），
          不会出现「调大了 OCR 超时却被单步超时提前掐断」的情况。
        </span>
      </div>

      <div className="field">
        <span className="field-label">滚动查找联系人</span>
        <div className="calibration-inputs">
          <label>
            <span>最多滚动次数</span>
            <input
              type="number"
              min={1}
              max={200}
              value={draft.max_scroll_attempts}
              onChange={(event) =>
                update("max_scroll_attempts", Math.max(1, Number(event.target.value) || 1))
              }
            />
          </label>
          <label>
            <span>每次格数</span>
            <input
              type="number"
              min={1}
              max={20}
              value={draft.scroll_notches_per_step}
              onChange={(event) =>
                update(
                  "scroll_notches_per_step",
                  Math.min(20, Math.max(1, Number(event.target.value) || 1)),
                )
              }
            />
          </label>
          <label>
            <span>完整扫描轮数</span>
            <input
              type="number"
              min={1}
              max={20}
              value={draft.max_search_sweeps}
              onChange={(event) =>
                update("max_search_sweeps", Math.min(20, Math.max(1, Number(event.target.value) || 1)))
              }
            />
          </label>
          <label>
            <span>滚动落点 横向</span>
            <input
              type="number"
              min={0}
              max={1}
              step={0.01}
              value={draft.scroll_anchor.x}
              onChange={(event) =>
                update("scroll_anchor", {
                  ...draft.scroll_anchor,
                  x: ratioFromInput(event.target.value, draft.scroll_anchor.x),
                })
              }
            />
          </label>
          <label>
            <span>滚动落点 纵向</span>
            <input
              type="number"
              min={0}
              max={1}
              step={0.01}
              value={draft.scroll_anchor.y}
              onChange={(event) =>
                update("scroll_anchor", {
                  ...draft.scroll_anchor,
                  y: ratioFromInput(event.target.value, draft.scroll_anchor.y),
                })
              }
            />
          </label>
          <label>
            <span>滚动停稳等待</span>
            <input
              type="number"
              min={0}
              step={50}
              value={draft.scroll_settle_ms}
              onChange={(event) =>
                update("scroll_settle_ms", Math.max(0, Math.round(Number(event.target.value) || 0)))
              }
            />
          </label>
        </div>
        <span className="field-hint">
          <strong>只对「列表扫描式」那条工作流生效</strong>——搜索式看的是顶部
          联想下拉，不滚这个列表。下面这些旋钮就是那条路要用的：
          目标不在当前可见范围时，会在联系人候选区向下滚动继续找。
          列表按「最近有消息」排序，扫描期间到达的新消息会把目标顶到最上面，
          而那一屏早被翻过去了——所以扫完一轮会<strong>回到顶部再扫一轮</strong>。
          完整扫描轮数就是那个上限；配成 1 就退回「只往下扫一遍」的老行为。
          滚完上限、或画面已经不再变化（滚到底了），任务会转人工处理。
        </span>
        <span className="field-hint">
          <strong>滚动落点</strong>是鼠标停在哪里滚（相对联系人候选区的比例），
          默认 <code>0.62 / 0.5</code> 即<strong>上下居中、左右偏右一点</strong>。
          不用正中心是因为候选区左边界把左侧图标栏和头像列一起圈了进来，
          正中心恰好压在头像列上；偏右落到名字那一列更稳。
          两个值都必须在 0–1 之间，填到范围外任务会直接报错而不是凑合着滚。
        </span>
        <span className="field-hint">
          <strong>滚动停稳等待</strong>（毫秒，<code>0</code> = 不等）是滚完之后
          「等画面停下来」的<strong>上限</strong>。客户端的列表滚动是带缓动的动画，
          滚完立刻截图会截到中间帧——文字是糊的、行是错位的，识别出来自然是乱的；
          而且「滚了一下画面没动 ⇒ 到底了」这个判断也会把「还没画完」当成
          「到底了」，于是提前收工、漏掉整段列表。这里是<strong>有界轮询</strong>：
          画面连续两帧一致就立刻继续，只有动画真的还在跑时才会等到上限，
          所以正常情况下开销只是多截一帧。
        </span>
      </div>
    </>
  );
}
