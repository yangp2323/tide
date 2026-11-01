"use client";
import { useCallback, useEffect, useState } from "react";
import { useFeedback } from "@/components/Feedback";
import { api, post } from "@/lib/api";
import { AI_ACT, AI_SOURCE, AI_STATUS, base, utc } from "@/lib/fmt";
import { Card, Empty, MsgBox, Skeleton, StateLabel, useAction } from "@/lib/ui";

type Cfg = {
  provider: string;
  model: string;
  base_url: string;
  api_key: string | null;
  interval: number;
  min_confidence: number;
  max_entries_per_day: number;
  max_daily_loss_pct: number;
  enabled: boolean;
};

type AiData = {
  config: Cfg;
  state: { running: boolean; last_run: string | null; next_run: string | null; last_error: string | null };
  entries_today: number;
  log: any[];
};

const DEFAULT_MODEL: Record<string, string> = { anthropic: "claude-opus-5-5", openai: "deepseek-chat" };

function DecisionRow({ r }: { r: any }) {
  const acts: any[] = r.actions || [];
  return (
    <div className="ai-log">
      <div className="meta">
        <span>{utc(r.time)}</span>
        <span>
          {AI_SOURCE[r.source] || r.source}
          {r.execute ? "" : " · 只看建议"}
        </span>
        <span>{r.model}</span>
        {r.seconds ? <span>{r.seconds}s</span> : null}
      </div>
      {r.error ? <div className="msg err">{r.error}</div> : null}
      {r.analysis ? <p>{r.analysis}</p> : null}
      {acts.length ? (
        acts.map((a, i) => (
          <div className="ai-act" key={i}>
            <span className={"st " + (a.status || "idea")}>{a.status ? AI_STATUS[a.status] : "建议"}</span>
            <b>
              {AI_ACT[a.action]} {base(a.pair)}
            </b>
            {r.source !== "mcp" ? <span className="muted num"> {(+a.confidence).toFixed(2)}</span> : null}
            　{a.reason}
            {a.note ? <div className="muted">{a.note}</div> : null}
          </div>
        ))
      ) : r.error ? null : (
        <div className="ai-act muted">观望，不操作</div>
      )}
    </div>
  );
}

export default function AiPage() {
  const { confirm } = useFeedback();
  const { busy, msg, setMsg, run } = useAction();
  const [data, setData] = useState<AiData | null>(null);
  const [err, setErr] = useState("");
  const [provider, setProvider] = useState("anthropic");
  const [model, setModel] = useState(DEFAULT_MODEL.anthropic);
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [interval, setInterval] = useState(240);
  const [conf, setConf] = useState(0.6);
  const [maxDay, setMaxDay] = useState(3);
  const [loss, setLoss] = useState(5);

  const fill = useCallback((c: Cfg) => {
    setProvider(c.provider);
    setModel(c.model);
    setBaseUrl(c.base_url || "");
    setInterval(c.interval);
    setConf(c.min_confidence);
    setMaxDay(c.max_entries_per_day);
    setLoss(c.max_daily_loss_pct);
  }, []);

  const load = useCallback(async () => {
    try {
      const r = await api<AiData>("/api/ai");
      setData(r);
      fill(r.config);
      setErr("");
    } catch (e: any) {
      setErr(e?.message || "加载失败");
    }
  }, [fill]);

  useEffect(() => {
    load();
  }, [load]);

  const c = data?.config;
  const on = !!c?.enabled;
  const st = data?.state;

  function switchProvider(p: string) {
    if (p === provider) return;
    setProvider(p);
    setModel(DEFAULT_MODEL[p] || "");
    setBaseUrl("");
  }

  async function saveCfg() {
    await run("save", async () => {
      await post("/api/ai/config", {
        provider,
        model,
        base_url: baseUrl,
        api_key: apiKey || null,
        interval,
        min_confidence: conf,
        max_entries_per_day: maxDay,
        max_daily_loss_pct: loss,
      });
      setApiKey("");
      await load();
      return { kind: "good", text: "已保存。" };
    });
  }

  async function toggle(want: boolean) {
    if (want) {
      const ok = await confirm({
        title: "开启 AI 交易？",
        body: "开启后由大模型决定开仓和平仓，用的是实盘真钱，均线策略停止自动开平仓。",
        ok: "开启",
        danger: true,
        expect: "YES",
      });
      if (!ok) return;
    }
    await run("enable", async () => {
      await post("/api/ai/enable", { enabled: want });
      await load();
      return { kind: "good", text: want ? "已开启 AI 交易。" : "已关闭，恢复均线策略。" };
    });
  }

  async function analyze(exec: boolean) {
    if (exec) {
      const ok = await confirm({
        title: "立即分析并执行？",
        body: "大模型会分析当前行情，并按它的决定用市价单开平仓（仍然经过硬性风控）。",
        ok: "分析并执行",
        danger: true,
      });
      if (!ok) return;
    }
    await run("run", async () => {
      const r = await post<any>(`/api/ai/run?execute=${exec}`);
      await load();
      return r.error ? { kind: "err", text: r.error } : { kind: "good", text: "分析完成，结果在下方决策记录。" };
    });
  }

  return (
    <>
      <div className="grid g2">
        <Card title="AI 交易" right={<StateLabel on={on}>{on ? "运行中" : "未开启"}</StateLabel>}>
          <p className="hint">
            开启后，<b>均线策略不再自己开平仓</b>，改由大模型在每根 K 线收盘后分析行情和持仓，决定开仓、平仓或观望。每笔金额、杠杆、价格反向 8%
            止损、移动止盈仍由系统执行，大模型改不了。
          </p>
          <div className="risk" style={{ marginTop: 12 }}>
            <div>
              <span>下次分析</span>
              <b>{st?.running ? "分析中…" : !on ? "—" : st?.next_run ? utc(st.next_run) : "等待调度"}</b>
            </div>
            <div>
              <span>今天 AI 开仓</span>
              <b>
                {data?.entries_today ?? "—"} / {c?.max_entries_per_day ?? "—"}
              </b>
            </div>
          </div>
          <div className="row">
            {!on ? (
              <button className="danger" onClick={() => toggle(true)} disabled={busy === "enable"}>
                开启 AI 交易
              </button>
            ) : (
              <button className="ghost" onClick={() => toggle(false)} disabled={busy === "enable"}>
                关闭，恢复均线策略
              </button>
            )}
            <button className="ghost" onClick={() => analyze(false)} disabled={busy === "run"}>
              {busy === "run" ? "分析中…" : "分析一次（只看建议）"}
            </button>
            {on && (
              <button className="ghost" onClick={() => analyze(true)} disabled={busy === "run"}>
                立即分析并执行
              </button>
            )}
          </div>
          <MsgBox m={msg} />
          {st?.last_error && !st.running ? <div className="msg warn">上次出错：{st.last_error}</div> : null}
        </Card>

        <Card
          title="大模型与风控"
          right={<StateLabel on={!!c?.api_key}>{c?.api_key ? `已保存 ${c.api_key}` : "未设置"}</StateLabel>}
        >
          {!c ? (
            <Skeleton lines={6} />
          ) : (
            <>
              <div className="seg" role="radiogroup" aria-label="接口">
                <button className={provider === "anthropic" ? "on" : ""} onClick={() => switchProvider("anthropic")}>
                  Claude
                </button>
                <button className={provider === "openai" ? "on" : ""} onClick={() => switchProvider("openai")}>
                  OpenAI 兼容
                </button>
              </div>
              <label className="f" htmlFor="aiModel">
                模型
              </label>
              <input id="aiModel" value={model} onChange={(e) => setModel(e.target.value)} spellCheck={false} autoComplete="off" />
              <label className="f" htmlFor="aiBase">
                接口地址（选填，用中转或 DeepSeek、通义等时填写）
              </label>
              <input
                id="aiBase"
                value={baseUrl}
                onChange={(e) => setBaseUrl(e.target.value)}
                spellCheck={false}
                autoComplete="off"
                placeholder={provider === "openai" ? "例如 https://api.deepseek.com" : "留空 = 官方 https://api.anthropic.com"}
              />
              <label className="f" htmlFor="aiKey">
                API Key
              </label>
              <input
                id="aiKey"
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                autoComplete="new-password"
                placeholder="已保存的话，不改就留空"
              />
              <div className="fields" style={{ gridTemplateColumns: "repeat(2,minmax(0,1fr))", gap: "0 16px" }}>
                <div>
                  <label className="f" htmlFor="aiInterval">
                    分析频率
                  </label>
                  <select id="aiInterval" value={interval} onChange={(e) => setInterval(+e.target.value)}>
                    <option value={240}>每 4 小时（K 线收盘后）</option>
                    <option value={60}>每小时</option>
                  </select>
                </div>
                <div>
                  <label className="f" htmlFor="aiConf">
                    最低置信度（低于不执行）
                  </label>
                  <input id="aiConf" type="number" step={0.05} min={0} max={1} value={conf} onChange={(e) => setConf(+e.target.value)} />
                </div>
                <div>
                  <label className="f" htmlFor="aiMaxDay">
                    每天最多开仓次数
                  </label>
                  <input id="aiMaxDay" type="number" step={1} min={1} max={20} value={maxDay} onChange={(e) => setMaxDay(+e.target.value)} />
                </div>
                <div>
                  <label className="f" htmlFor="aiLoss">
                    当日亏损达到 % 停止开仓
                  </label>
                  <input id="aiLoss" type="number" step={0.5} min={0.5} max={50} value={loss} onChange={(e) => setLoss(+e.target.value)} />
                </div>
              </div>
              <div className="row">
                <button onClick={saveCfg} disabled={busy === "save"}>
                  {busy === "save" ? "保存中…" : "保存"}
                </button>
              </div>
              <MsgBox m={msg} />
            </>
          )}
        </Card>
      </div>

      <Card
        title="决策记录"
        note="最近 20 次，最新在最上面"
        style={{ marginTop: 16 }}
        right={
          <button className="ghost sm" onClick={load} disabled={busy === "load"}>
            刷新
          </button>
        }
      >
        {err ? (
          <Empty>{err}</Empty>
        ) : !data ? (
          <Skeleton lines={5} />
        ) : data.log.length ? (
          data.log.map((r) => <DecisionRow key={r.time + r.source} r={r} />)
        ) : (
          <Empty>还没有记录</Empty>
        )}
      </Card>
    </>
  );
}
