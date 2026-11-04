"use client";
import { useCallback, useEffect, useState } from "react";
import { api } from "@/lib/api";
import { AiData, HOOK_LABEL, UsageDay } from "@/lib/ai";
import { AI_ACT, AI_SOURCE, AI_STATUS, base, utc } from "@/lib/fmt";
import { Card, Empty, Skeleton } from "@/lib/ui";

type Kind = "ok" | "bad" | "warn" | "idea" | "idle";

// One line that says what this call amounted to, so the list can be scanned.
function outcome(r: any): { kind: Kind; label: string } {
  const acts: any[] = r.actions || [];
  if (r.error) return { kind: "bad", label: "出错" };
  if (r.source === "review") {
    const n = (r.changes || []).filter((c: any) => c.status === "applied").length;
    return n ? { kind: "ok", label: `写入 ${n} 条笔记` } : { kind: "idle", label: "没有改动" };
  }
  if (!acts.length) return { kind: "idle", label: "观望" };
  if (!r.execute) return { kind: "idea", label: `只看建议 ${acts.length} 条` };
  const n = (s: string) => acts.filter((a) => a.status === s).length;
  const parts = [
    n("executed") && `已执行 ${n("executed")}`,
    n("blocked") && `拦截 ${n("blocked")}`,
    n("failed") && `失败 ${n("failed")}`,
  ].filter(Boolean);
  const kind: Kind = n("executed") ? "ok" : n("failed") ? "bad" : "warn";
  return { kind, label: parts.join(" · ") || "未执行" };
}

function trigger(r: any) {
  return (r.trigger?.events || [])
    .map((e: any) => `${HOOK_LABEL[e.type] || e.type}${e.pair ? " " + base(e.pair) : ""}${e.message ? "：" + String(e.message).slice(0, 40) : ""}`)
    .join("；");
}

function DecisionRow({ r }: { r: any }) {
  const acts: any[] = r.actions || [];
  const o = outcome(r);
  const why = trigger(r);
  const u = r.usage || {};
  const detail = r.analysis || r.memory;
  return (
    <div className={`call ${o.kind}`}>
      <div className="head">
        <b className="when">{utc(r.time)}</b>
        <span className={`tag src-${r.source}`}>{AI_SOURCE[r.source] || r.source}</span>
        <span className={`res ${o.kind}`}>{o.label}</span>
        {why ? <span className="why">{why}</span> : null}
      </div>
      {r.error ? <div className="msg err">{r.error}</div> : null}
      {(r.changes || []).map((c: any, i: number) => (
        <div className="act" key={`c${i}`}>
          <span className={"st " + (c.status === "applied" ? "executed" : "failed")}>{c.status === "applied" ? "已写入" : "已跳过"}</span>
          <b>
            {{ add: "新增笔记", update: "更新笔记", delete: "删除笔记" }[c.op as string] || c.op} {c.title || ""}
          </b>
          {c.note ? <span className="sub">{c.note}</span> : null}
        </div>
      ))}
      {acts.map((a, i) => (
        <div className="act" key={i}>
          <span className={"st " + (a.status || "idea")}>{a.status ? AI_STATUS[a.status] : "建议"}</span>
          <b>
            {AI_ACT[a.action] || a.action} {base(a.pair)}
          </b>
          {r.source !== "mcp" && a.confidence != null ? <span className="conf num">{(+a.confidence).toFixed(2)}</span> : null}
          <span className="sub">
            {a.reason}
            {a.note ? `（${a.note}）` : ""}
          </span>
        </div>
      ))}
      {detail ? (
        <details className="more">
          <summary>分析与笔记</summary>
          {r.analysis ? <p>{r.analysis}</p> : null}
          {r.memory ? <p className="muted">给下一轮的笔记：{r.memory}</p> : null}
        </details>
      ) : null}
      <div className="tech">
        {r.channel ? `${r.channel} · ` : ""}
        {r.model}
        {r.seconds ? ` · ${r.seconds}s` : ""}
        {u.cache_read_input_tokens || u.cache_creation_input_tokens
          ? ` · 缓存命中 ${u.cache_read_input_tokens || 0} / 写入 ${u.cache_creation_input_tokens || 0} / 未缓存 ${u.input_tokens || 0}`
          : ""}
        {r.execute ? "" : " · 影子或只看建议"}
      </div>
    </div>
  );
}

const tok = (n: number) => (n >= 1e6 ? (n / 1e6).toFixed(2) + "M" : n >= 1e3 ? (n / 1e3).toFixed(1) + "k" : String(n));

function UsageCard() {
  const [days, setDays] = useState<UsageDay[] | null>(null);
  useEffect(() => {
    api<UsageDay[]>("/api/ai/usage?limit=14").then(setDays).catch(() => setDays([]));
  }, []);
  if (!days?.length) return null;
  return (
    <Card title="调用与用量" note="最近 14 天，只统计 token，不换算金额（价格取决于你的模型和渠道）">
      <div style={{ overflowX: "auto" }}>
        <table className="tbl">
          <thead>
            <tr>
              <th>日期（UTC）</th>
              <th>调用</th>
              <th>输入</th>
              <th>缓存命中</th>
              <th>输出</th>
            </tr>
          </thead>
          <tbody>
            {days.map((d) => (
              <tr key={d.date}>
                <td className="t">{d.date.slice(5)}</td>
                <td title={Object.entries(d.by_source).map(([k, v]) => `${AI_SOURCE[k] || k} ${v}`).join("，")}>
                  {d.calls}
                  {d.failed ? ` (失败 ${d.failed})` : ""}
                </td>
                <td>{tok(d.input)}</td>
                <td>{d.input ? `${Math.round((d.cache_read / d.input) * 100)}%` : "—"}</td>
                <td>{tok(d.output)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Card>
  );
}

export default function DecisionsPage() {
  const [data, setData] = useState<AiData | null>(null);
  const [err, setErr] = useState("");
  const [quiet, setQuiet] = useState(false);

  const load = useCallback(async () => {
    try {
      setData(await api<AiData>("/api/ai"));
      setErr("");
    } catch (e: any) {
      setErr(e?.message || "加载失败");
    }
  }, []);

  useEffect(() => {
    load();
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") load();
    }, 15000);
    return () => clearInterval(timer);
  }, [load]);

  const rows = (data?.log || []).filter((r: any) => !quiet || outcome(r).kind !== "idle");
  return (
    <>
    <UsageCard />
    <Card
      title="决策记录"
      note="每次大模型调用一条，最近 20 次，最新在最上面。开关和运行方式在「交易策略」页"
      right={
        <>
          <label className="toggle sm">
            <input type="checkbox" checked={quiet} onChange={(e) => setQuiet(e.target.checked)} />
            <span>隐藏观望</span>
          </label>
          <button className="ghost sm" onClick={() => load()}>
            刷新
          </button>
        </>
      }
    >
      {err ? (
        <Empty>{err}</Empty>
      ) : !data ? (
        <Skeleton lines={5} />
      ) : rows.length ? (
        <div className="calls">
          {rows.map((r: any) => (
            <DecisionRow key={r.time + r.source} r={r} />
          ))}
        </div>
      ) : (
        <Empty>{data.log.length ? "没有需要关注的记录" : "还没有记录"}</Empty>
      )}
    </Card>
    </>
  );
}
