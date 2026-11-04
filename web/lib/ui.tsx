"use client";
import { useCallback, useState } from "react";
import { useOv } from "./store";

export type Msg = { kind: "err" | "good" | "warn"; text: string } | null;

export function MsgBox({ m }: { m: Msg }) {
  if (!m) return null;
  return (
    <div className={"msg " + m.kind} role={m.kind === "err" ? "alert" : "status"}>
      {m.text}
    </div>
  );
}

export function Spin() {
  return <span className="busy" aria-hidden="true" />;
}

/** 按钮点击后的统一处理：禁用 + 转圈 + 报错显示 + 完成后刷新概览 */
export function useAction() {
  const { refresh } = useOv();
  const [busy, setBusy] = useState("");
  const [msg, setMsg] = useState<Msg>(null);

  const run = useCallback(
    async (key: string, fn: () => Promise<Msg | void>, after = true) => {
      setBusy(key);
      setMsg(null);
      try {
        const r = await fn();
        if (r) setMsg(r);
      } catch (e: any) {
        setMsg({ kind: "err", text: e?.message || String(e) });
      } finally {
        setBusy("");
        if (after) refresh();
      }
    },
    [refresh],
  );

  return { busy, msg, setMsg, run };
}

export function Card({
  title,
  note,
  right,
  children,
  className = "",
  style,
}: {
  title?: React.ReactNode;
  note?: React.ReactNode;
  right?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
  style?: React.CSSProperties;
}) {
  const hasHead = title != null || note != null || right != null;
  return (
    <section className={"card " + className} style={style}>
      {hasHead && (
        <header>
          {title != null && <h2>{title}</h2>}
          {note != null && <span className="note">{note}</span>}
          <span className="sp" />
          {right}
        </header>
      )}
      {children}
    </section>
  );
}

export const Empty = ({ children }: { children: React.ReactNode }) => <div className="empty">{children}</div>;

export const SideTag = ({ short }: { short?: boolean }) =>
  short ? <span className="side short">空</span> : <span className="side long">多</span>;

export const StateLabel = ({ on, children }: { on?: boolean; children: React.ReactNode }) => (
  <span className={"state" + (on ? " ok" : "")}>{children}</span>
);

/** 首屏骨架，避免空表格闪一下 */
export function Skeleton({ lines = 3, height = 14 }: { lines?: number; height?: number }) {
  return (
    <div className="skel" aria-busy="true" aria-live="polite">
      {Array.from({ length: lines }).map((_, i) => (
        <span key={i} style={{ height, width: `${88 - i * 12}%` }} />
      ))}
    </div>
  );
}
