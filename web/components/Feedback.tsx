"use client";
import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";

type Kind = "good" | "err" | "warn";

type ConfirmOpts = {
  title: string;
  body?: React.ReactNode;
  ok?: string;
  cancel?: string;
  danger?: boolean;
  /** 高风险操作要求手动输入这段文字才能确认 */
  expect?: string;
};

type ToastItem = { id: number; text: string; kind: Kind };

type Ctx = {
  confirm: (o: ConfirmOpts) => Promise<boolean>;
  toast: (text: string, kind?: Kind) => void;
};

const C = createContext<Ctx>({ confirm: async () => false, toast: () => {} });
export const useFeedback = () => useContext(C);

/** 统一替代 window.confirm / alert：移动端可用，观感一致，还能要求输入确认词 */
export function FeedbackProvider({ children }: { children: React.ReactNode }) {
  const [ask, setAsk] = useState<ConfirmOpts | null>(null);
  const [text, setText] = useState("");
  const [toasts, setToasts] = useState<ToastItem[]>([]);
  const resolver = useRef<((v: boolean) => void) | null>(null);

  const close = useCallback((ok: boolean) => {
    setAsk(null);
    setText("");
    resolver.current?.(ok);
    resolver.current = null;
  }, []);

  const confirm = useCallback(
    (o: ConfirmOpts) =>
      new Promise<boolean>((resolve) => {
        resolver.current?.(false);
        resolver.current = resolve;
        setText("");
        setAsk(o);
      }),
    [],
  );

  const toast = useCallback((msg: string, kind: Kind = "good") => {
    const id = Date.now() + Math.random();
    setToasts((t) => [...t, { id, text: msg, kind }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), 3200);
  }, []);

  useEffect(() => {
    if (!ask) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [ask, close]);

  const blocked = !!ask?.expect && text.trim() !== ask.expect;

  return (
    <C.Provider value={{ confirm, toast }}>
      {children}
      {ask && (
        <div
          className="scrim"
          onMouseDown={(e) => {
            if (e.target === e.currentTarget) close(false);
          }}
        >
          <div className="modal" role="dialog" aria-modal="true" aria-labelledby="ask-title">
            <h3 id="ask-title">{ask.title}</h3>
            {ask.body && <div className="modal-body">{ask.body}</div>}
            {ask.expect && (
              <input
                autoFocus
                value={text}
                placeholder={`输入 ${ask.expect} 确认`}
                onChange={(e) => setText(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !blocked) close(true);
                }}
                aria-label={`输入 ${ask.expect} 确认`}
              />
            )}
            <div className="row modal-acts">
              <button className="ghost" onClick={() => close(false)}>
                {ask.cancel ?? "取消"}
              </button>
              <button
                autoFocus={!ask.expect}
                className={ask.danger ? "danger" : ""}
                disabled={blocked}
                onClick={() => close(true)}
              >
                {ask.ok ?? "确定"}
              </button>
            </div>
          </div>
        </div>
      )}
      <div className="toasts" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} className={"toast " + t.kind}>
            {t.text}
          </div>
        ))}
      </div>
    </C.Provider>
  );
}
