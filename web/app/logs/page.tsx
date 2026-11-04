"use client";
import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "@/lib/api";
import { Card, Empty, Skeleton } from "@/lib/ui";

type Row = [string, string, number, string, string];

const BAD = ["ERROR", "CRITICAL"];

export default function LogsPage() {
  const [rows, setRows] = useState<Row[] | null>(null);
  const [err, setErr] = useState("");
  const [onlyErr, setOnlyErr] = useState(false);
  const [auto, setAuto] = useState(true);
  const [loading, setLoading] = useState(false);
  const box = useRef<HTMLDivElement>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const r = await api<any>("/api/logs?limit=300");
      setRows(r.logs || []);
      setErr("");
    } catch (e: any) {
      setErr(e?.message || "加载失败");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    if (!auto) return;
    const t = setInterval(() => {
      if (document.visibilityState === "visible") load();
    }, 10000);
    return () => clearInterval(t);
  }, [auto, load]);

  // 只有原本就在底部时才跟着滚；向上翻看历史时不要被自动刷新拉回去
  const stick = useRef(true);
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const onScroll = () => {
      stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
    };
    el.addEventListener("scroll", onScroll);
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  useEffect(() => {
    if (rows && box.current && stick.current) box.current.scrollTop = box.current.scrollHeight;
  }, [rows]);

  const shown = (rows || []).filter((l) => !onlyErr || ["WARNING", "ERROR", "CRITICAL"].includes(l[3]));

  return (
    <Card
      title="运行日志"
      note="最近 300 条，最新在最下面"
      right={
        <div className="row" style={{ marginTop: 0 }}>
          <label className="toggle" style={{ fontSize: 13 }}>
            <input type="checkbox" checked={onlyErr} onChange={(e) => setOnlyErr(e.target.checked)} />
            <span>只看警告和错误</span>
          </label>
          <label className="toggle" style={{ fontSize: 13 }}>
            <input type="checkbox" checked={auto} onChange={(e) => setAuto(e.target.checked)} />
            <span>自动刷新</span>
          </label>
          <button className="ghost sm" onClick={load} disabled={loading}>
            {loading ? "加载中…" : "刷新"}
          </button>
        </div>
      }
    >
      {err ? (
        <Empty>{err}</Empty>
      ) : !rows ? (
        <Skeleton lines={8} />
      ) : (
        <div className="logbox" ref={box} role="log" aria-live="off">
          {shown.length ? (
            shown.map((l, i) => (
              <div key={i}>
                <span className="t">{String(l[0]).slice(5)}</span>{" "}
                <span className={l[3] === "WARNING" ? "w" : BAD.includes(l[3]) ? "e" : ""}>
                  {String(l[3]).padEnd(7)} {l[4]}
                </span>
              </div>
            ))
          ) : (
            <div>没有符合条件的日志</div>
          )}
        </div>
      )}
    </Card>
  );
}
