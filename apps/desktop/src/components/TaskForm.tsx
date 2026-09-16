import { useState } from "react";

import type { StartTaskRequest } from "../types";

interface Props {
  disabled: boolean;
  onStart: (request: StartTaskRequest) => void;
  error: string | null;
}

export function TaskForm({ disabled, onStart, error }: Props) {
  const [contact, setContact] = useState("");
  const [text, setText] = useState("");
  const [operator, setOperator] = useState("");

  const canSubmit = !disabled && contact.trim() !== "" && text.trim() !== "";

  return (
    <section className="panel">
      <h2>新建发送任务</h2>

      <label className="field">
        <span className="field-label">外部联系人名称</span>
        <input
          type="text"
          value={contact}
          placeholder="必须与聊天页显示的名称逐字一致"
          onChange={(event) => setContact(event.target.value)}
        />
      </label>

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
