import type { RuntimeConfig } from "../types";

interface Props {
  draft: RuntimeConfig;
  onPatch: (patch: Partial<RuntimeConfig>) => void;
}

/**
 * 「逐字输入与靶标文字」这一组。
 *
 * ## 为什么单独成文件
 *
 * 这四个值是**同一类**：都"随客户端版本变"，所以做成配置而不是写死在代码里；
 * 而且**改动理由相同**——换一个靶标（或客户端升级）时，要动的就是这几处。
 * 它们散布在主文件里时，"这次升级要改哪几个字"得靠搜关键字才知道，
 * 归到一起以后打开这一个文件就够了。
 *
 * ★ 全是**配置**，改完要点「保存配置」。
 */
export function TypingTextSection({ draft, onPatch }: Props) {
  // 改草稿的字段。本地适配器——只为让下面的调用点短一点，判据不在这里。
  const update = <K extends keyof RuntimeConfig>(key: K, value: RuntimeConfig[K]) => {
    onPatch({ [key]: value } as Partial<RuntimeConfig>);
  };

  return (
    /* ── 逐字输入与靶标文字 ──────────────────────────────────────
        这两类值都是"随客户端版本变"的，所以做成配置而不是写死在代码里。
        它们合在一起是因为**改动理由相同**：换一个靶标（或客户端升级）时，
        要动的就是这几个字与这个间隔。 */
    <section className="calibration">
      <div className="calibration-head">
        <h3>逐字输入与靶标文字</h3>
      </div>

      <label className="field">
        <span className="field-label">逐字输入间隔（毫秒）</span>
        <input
          type="number"
          min={0}
          step={5}
          value={draft.typing_interval_ms}
          onChange={(event) =>
            update("typing_interval_ms", Math.max(0, Math.round(Number(event.target.value) || 0)))
          }
        />
        <span className="field-hint">
          搜索词与消息正文都是<strong>逐字</strong>输入的，这是字符之间的间隔
          （<code>0</code> = 不留间隔）。搜索框是联想式的：一次性灌进去的字符
          会让联想请求互相打断，下拉列表只按第一个字符的结果定格——
          现象是"搜出来的东西不对"，不会让人想到是<strong>输入太快</strong>。
          机器慢就调大，嫌慢就调小。
        </span>
      </label>

      <label className="field">
        <span className="field-label">资料页「进入聊天」入口的文字</span>
        <input
          type="text"
          value={draft.profile_chat_entry_text}
          onChange={(event) => update("profile_chat_entry_text", event.target.value)}
        />
        <span className="field-hint">
          资料页滚到底之后要点的那个入口上写的字（默认「发消息」）。
          这一项<strong>不能留空</strong>：空文字在「包含」判断里会匹配到任何一行，
          结果不是"找不到"而是<strong>找错</strong>。找不到时任务会转人工并说明原因。
        </span>
      </label>

      <label className="field">
        <span className="field-label">搜索下拉里「联系人」分组的标题</span>
        <input
          type="text"
          value={draft.search_contact_group_label}
          onChange={(event) => update("search_contact_group_label", event.target.value)}
        />
        <span className="field-hint">
          下拉是<strong>分组</strong>的（联系人 / 聊天记录 / 群聊…），
          只有「联系人」这一组下面才是人。编排时会先找这个标题，
          再<strong>只往它下方</strong>找匹配输入词的那一行——
          否则会把「聊天记录里提到这个名字」当成联系人，点进去就是别的地方。
          同样不能留空。
        </span>
      </label>
    </section>
  );
}
