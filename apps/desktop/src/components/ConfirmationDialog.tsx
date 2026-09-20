import { useEffect, useState } from "react";

import type { ConfirmationRequest } from "../types";

interface Props {
  request: ConfirmationRequest;
  onDecide: (approved: boolean, reason?: string) => void;
}

export function ConfirmationDialog({ request, onDecide }: Props) {
  const [remaining, setRemaining] = useState(() =>
    Math.max(0, Math.ceil(request.expires_in_ms / 1000)),
  );

  useEffect(() => {
    setRemaining(Math.max(0, Math.ceil(request.expires_in_ms / 1000)));
    const timer = window.setInterval(() => {
      setRemaining((value) => (value > 0 ? value - 1 : 0));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [request.task_id, request.expires_in_ms]);

  const expired = remaining <= 0;

  return (
    <div className="modal-backdrop" role="dialog" aria-modal="true">
      <div className="modal">
        <h2>发送前确认</h2>
        <p className="modal-warning">
          请核对收件人与内容。只有确认之后，工作流才会继续发送。
        </p>

        <dl className="confirm-grid">
          <dt>收件人</dt>
          <dd className="confirm-strong">{request.external_contact_name}</dd>
          <dt>消息内容</dt>
          <dd className="confirm-message">{request.text}</dd>
        </dl>

        <p className={expired ? "countdown is-expired" : "countdown"}>
          {expired ? "确认已过期，任务将转入人工处理" : `剩余 ${remaining} 秒`}
        </p>

        <div className="modal-actions">
          <button
            className="ghost"
            disabled={expired}
            onClick={() => onDecide(false, "操作者拒绝发送")}
          >
            拒绝
          </button>
          <button className="primary" disabled={expired} onClick={() => onDecide(true)}>
            确认发送
          </button>
        </div>
      </div>
    </div>
  );
}
