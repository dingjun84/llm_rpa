import { useState } from "react";

import type { TaskFormValues } from "../types";

interface Props {
  disabled: boolean;
  onStart: (values: TaskFormValues) => void;
  error: string | null;
  /**
   * 当前工作流要不要「外部联系人名称」/「消息正文」。
   *
   * ★★ 判据在**后端**（`runtime::workflow_inputs`，经 `workflow_requirements`
   * 下发），这里只渲染。自己判一句"只做导航不用填"的话，判据就有两处了。
   *
   * `null` = 还没问到 / 问不到。**这时一律不拦**：
   * 界面拿不到判据时宁可放行，让命令层去拒绝并说清原因——
   * **拒绝是看得见的，点不动是看不见的**。
   */
  needsContact: boolean | null;
  needsMessage: boolean | null;
}

/**
 * 「新建发送任务」。
 *
 * ★ 它**不碰「走哪条路」**（工作流 / 导航目标）：那是 `App` 里的一份运行参数，
 * 由 `App.handleStart` 在提交时补进请求里。理由见 `TaskFormValues` 的注释——
 * 表单只管"发给谁、发什么"，"怎么跑"不归它管。
 *
 * ## 「只做导航」为什么整块输入都不显示
 *
 * 那条工作流**不找任何人、也发不出消息**（`Workflow::NavigateOnly`），
 * 这两个框对它没有意义。以前它们照样是必填：空着时「开始任务」是灰的、
 * 点了**什么都不发生、也不说为什么**（2026-09-20 实测）。
 * 光把按钮放开还不够——留着一个填了也没用的框，下一个人还会去填它。
 */
export function TaskForm({
  disabled,
  onStart,
  error,
  needsContact,
  needsMessage,
}: Props) {
  const [contact, setContact] = useState("");
  const [text, setText] = useState("");
  const [operator, setOperator] = useState("");

  const showContact = needsContact !== false;
  const showMessage = needsMessage !== false;

  // 只有**明确知道要填**时才拿它当门槛（`null` 放行，见 `needsContact` 的说明）。
  const canSubmit =
    !disabled &&
    (needsContact !== true || contact.trim() !== "") &&
    (needsMessage !== true || text.trim() !== "");

  return (
    <section className="panel">
      <h2>新建发送任务</h2>

      {!showContact && !showMessage && (
        <p className="notice">
          这一条工作流<strong>不查找任何人、也不发送消息</strong>
          （它只负责"找到图标并点它"），所以下面不需要填联系人与正文——
          要改的是「要点哪一个图标」，在下面「工作流」那一组里选。
        </p>
      )}

      {showContact && (
        <label className="field">
          <span className="field-label">外部联系人名称</span>
          <input
            type="text"
            value={contact}
            placeholder="必须与聊天页显示的名称逐字一致"
            onChange={(event) => setContact(event.target.value)}
          />
        </label>
      )}

      {showMessage && (
        <label className="field">
          <span className="field-label">消息正文</span>
          <textarea
            rows={4}
            value={text}
            placeholder="仅支持单条纯文本"
            onChange={(event) => setText(event.target.value)}
          />
          <span className="field-hint">{text.length} 字</span>
        </label>
      )}

      <label className="field">
        <span className="field-label">操作者（可选）</span>
        <input
          type="text"
          value={operator}
          placeholder="会写入审计记录"
          onChange={(event) => setOperator(event.target.value)}
        />
      </label>

      <button
        className="primary"
        disabled={!canSubmit}
        onClick={() =>
          onStart({
            external_contact_name: contact.trim(),
            text: text.trim(),
            created_by: operator.trim() === "" ? null : operator.trim(),
          })
        }
      >
        {disabled ? "已有任务正在执行" : "开始任务"}
      </button>

      {error && <p className="error-line">{error}</p>}
    </section>
  );
}
