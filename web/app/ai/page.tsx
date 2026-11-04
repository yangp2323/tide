"use client";
import Link from "next/link";
import { useCallback, useEffect, useState } from "react";
import { useFeedback } from "@/components/Feedback";
import { api, del, post } from "@/lib/api";
import { AiData, HOOK_LABEL, Hooks } from "@/lib/ai";
import { AI_ACT, AI_SOURCE, AI_STATUS, base, utc } from "@/lib/fmt";
import { useOv } from "@/lib/store";
import { Card, Empty, MsgBox, Skeleton, StateLabel, useAction } from "@/lib/ui";

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
        <span>{r.channel ? `${r.channel} · ` : ""}{r.model}</span>
        {r.seconds ? <span>{r.seconds}s</span> : null}
      </div>
      {r.error ? <div className="msg err">{r.error}</div> : null}
      {r.trigger?.events?.length ? (
        <p className="muted">
          触发：
          {r.trigger.events
            .map((e: any) => `${HOOK_LABEL[e.type] || e.type}${e.pair ? " " + base(e.pair) : ""}${e.message ? "：" + String(e.message).slice(0, 60) : ""}`)
            .join("；")}
        </p>
      ) : null}
      {r.analysis ? <p>{r.analysis}</p> : null}
      {(r.changes || []).map((c: any, i: number) => (
        <div className="ai-act" key={`c${i}`}>
          <span className={"st " + (c.status === "applied" ? "executed" : "failed")}>{c.status === "applied" ? "已写入" : "已跳过"}</span>
          <b>
            {{ add: "新增笔记", update: "更新笔记", delete: "删除笔记" }[c.op as string] || c.op} {c.title || ""}
          </b>
          {c.note ? <div className="muted">{c.note}</div> : null}
        </div>
      ))}
      {r.memory ? <p className="muted">给下一轮的笔记：{r.memory}</p> : null}
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
      ) : r.error || r.source === "review" ? null : (
        <div className="ai-act muted">观望，不操作</div>
      )}
    </div>
  );
}

export default function AiPage() {
  const { d } = useOv();
  const { confirm } = useFeedback();
  const { busy, msg, run } = useAction();
  const [data, setData] = useState<AiData | null>(null);
  const [err, setErr] = useState("");
  const [freq, setFreq] = useState(240);
  const [delay, setDelay] = useState(180);
  const [shadow, setShadow] = useState(false);
  const [review, setReview] = useState(true);
  const [hk, setHk] = useState<Hooks | null>(null);
  const [newToken, setNewToken] = useState("");

  const load = useCallback(async () => {
    try {
      const r = await api<AiData>("/api/ai");
      setData(r);
      setFreq(r.config.interval);
      setDelay(r.config.delay_seconds ?? 180);
      setShadow(!!r.config.shadow);
      setReview(r.config.review !== false);
      setHk(r.config.hooks ?? null);
      setErr("");
    } catch (e: any) {
      setErr(e?.message || "加载失败");
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  // Refresh decision state without replacing settings the user is editing.
  useEffect(() => {
    const timer = setInterval(async () => {
      if (document.visibilityState !== "visible") return;
      try {
        const r = await api<AiData>("/api/ai");
        setData((previous) => previous ? { ...previous, state: r.state, position_count: r.position_count, max_open: r.max_open, log: r.log } : previous);
      } catch { /* The shared overview already reports connection failures. */ }
    }, 15000);
    return () => clearInterval(timer);
  }, []);

  const c = data?.config;
  const on = !!c?.enabled;
  const st = data?.state;
  const channel = c?.channels?.find((x) => x.id === c.active_channel);

  async function saveSchedule() {
    await run("save", async () => {
      if (!Number.isInteger(freq) || freq < 1 || freq > 1440) throw new Error("分析间隔须为 1 到 1440 分钟");
      if (!Number.isInteger(delay) || delay < 0 || delay >= freq * 60) throw new Error("延迟须为非负整数秒，且小于分析间隔");
      await post("/api/ai/config", { interval: freq, delay_seconds: delay, shadow, review });
      await load();
      return { kind: "good", text: "已保存。" };
    });
  }

  async function saveHooks() {
    if (!hk) return;
    await run("hooks", async () => {
      const { webhook_token, webhook_set, ...settings } = hk;
      await post("/api/ai/config", { hooks: settings });
      await load();
      return { kind: "good", text: "已保存。" };
    });
  }

  async function makeToken() {
    if (hk?.webhook_set) {
      const ok = await confirm({ title: "重新生成 Webhook 地址？", body: "旧地址会立即失效，已经配置了旧地址的外部系统要改成新地址。", ok: "重新生成", danger: true });
      if (!ok) return;
    }
    await run("token", async () => {
      const r = await post<any>("/api/ai/hooks/token");
      setNewToken(r.token);
      await load();
      return { kind: "good", text: "已生成。完整地址只显示这一次，请现在复制保存。" };
    });
  }

  async function dropToken() {
    const ok = await confirm({ title: "停用 Webhook？", body: "现有地址立即失效。", ok: "停用", danger: true });
    if (!ok) return;
    await run("token", async () => {
      await del("/api/ai/hooks/token");
      setNewToken("");
      await load();
      return { kind: "good", text: "已停用。" };
    });
  }

  async function toggle(want: boolean) {
    if (want) {
      const ok = await confirm({
        title: "开启 AI 交易？",
        body: "开启后由大模型决定开仓和平仓，用的是实盘真钱。止损和止盈也由大模型设置，系统负责执行。",
        ok: "开启",
        danger: true,
        expect: "YES",
      });
      if (!ok) return;
    }
    await run("enable", async () => {
      await post("/api/ai/enable", { enabled: want });
      await load();
      return { kind: "good", text: want ? "已开启 AI 交易。" : "已关闭自动交易。" };
    });
  }

  async function reviewNow() {
    await run("review", async () => {
      const r = await post<any>("/api/ai/review");
      await load();
      return r.error ? { kind: "err", text: r.error } : { kind: "good", text: "复盘完成，结果在下方决策记录，笔记在知识库页。" };
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
            开启后，<b>所有自动开平仓都由大模型决定</b>：方向、同时持有几笔、每笔仓位和杠杆、止损和止盈都由它定，系统只设硬上限并负责执行，两次分析之间还会按止损距离的一半自动追踪止损作保底。使用哪个模型在「大模型」页选，市场、币种、风控在「交易策略」页设。
          </p>
          <div className="risk" style={{ marginTop: 12 }}>
            <div>
              <span>下次分析</span>
              <b>{st?.running ? "分析中…" : !on ? "—" : st?.next_run ? utc(st.next_run) : "等待调度"}</b>
            </div>
            <div>
              <span>AI 当前持仓</span>
              <b>
                {data?.position_count ?? "—"} / {data?.max_open ?? "—"}
              </b>
            </div>
            <div>
              <span>使用的渠道</span>
              <b>
                <Link href="/model/">{channel ? `${channel.name} · ${channel.model}` : "—"}</Link>
              </b>
            </div>
          </div>
          <p className="hint">同时最多持有 {d.max_open_limit ?? 5} 笔是系统上限，不要求开满，平仓后释放名额。</p>
          <div className="row">
            {!on ? (
              <button className="danger" onClick={() => toggle(true)} disabled={busy === "enable"}>
                开启 AI 交易
              </button>
            ) : (
              <button className="ghost" onClick={() => toggle(false)} disabled={busy === "enable"}>
                关闭自动交易
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
            <button className="ghost" onClick={reviewNow} disabled={busy === "review" || busy === "run"}>
              {busy === "review" ? "复盘中…" : "立即复盘"}
            </button>
          </div>
          <MsgBox m={msg} />
          {st?.last_error && !st.running ? <div className="msg warn">上次出错：{st.last_error}</div> : null}
        </Card>

        <Card title="运行方式" note="何时分析、是否下单、是否复盘" className="ai-settings">
          {!c ? (
            <Skeleton lines={4} />
          ) : (
            <>
              <div className="ai-fields">
                <div>
                  <label className="f" htmlFor="aiInterval">
                    分析间隔（分钟）
                  </label>
                  <input id="aiInterval" type="number" min={1} max={1440} step={1} value={freq} onChange={(e) => setFreq(+e.target.value)} list="aiIntervalPresets" />
                  <datalist id="aiIntervalPresets">{[1,5,15,30,60,120,240,1440].map((minutes) => <option key={minutes} value={minutes} />)}</datalist>
                  <p className="hint">可自定义 1–1440 分钟，例如每 5、15、30 分钟一次。</p>
                </div>
                <div>
                  <label className="f" htmlFor="aiDelay">计划时间后延迟（秒）</label>
                  <input id="aiDelay" type="number" min={0} max={Math.max(0, freq * 60 - 1)} step={1} value={delay} onChange={(e) => setDelay(+e.target.value)} />
                  <p className="hint">0 表示到点即分析；延迟须小于分析间隔。修改后从下一个计划时间生效，不补跑。行情仍使用 4 小时已收盘 K 线。</p>
                </div>
              </div>
              <label className="toggle" style={{ marginTop: 12 }}>
                <input type="checkbox" checked={review} onChange={(e) => setReview(e.target.checked)} />
                <span>AI 复盘学习</span>
              </label>
              <p className="hint">
                新平仓的交易累计 3 笔（或 1 笔且距上次复盘超过一天）后，AI 自动复盘：看胜率、各种出场方式的盈亏、币种方向和持仓时长的统计，把有数据支持的观察写进知识库（最多 12 条，只能改自己写的）。每次复盘会多用一次模型调用；你可以在知识库页停用或删除它写的笔记。
              </p>
              <label className="toggle" style={{ marginTop: 12 }}>
                <input type="checkbox" checked={shadow} onChange={(e) => setShadow(e.target.checked)} />
                <span>影子模式</span>
              </label>
              <p className="hint">开启后按计划分析并记录决策，但不下任何单，用来观察模型质量。</p>
              <div className="row">
                <button onClick={saveSchedule} disabled={busy === "save"}>
                  {busy === "save" ? "保存中…" : "保存"}
                </button>
              </div>
            </>
          )}
        </Card>
      </div>

      <Card title="事件触发" note="不等定时分析，出事时主动叫醒 AI" className="ai-settings">
        {!hk ? (
          <Skeleton lines={4} />
        ) : (
          <>
            <p className="hint">
              事件触发只是让 AI 提前分析一次，它能做的事和定时分析完全一样，仍受全部风控限制。两次分析之间至少隔一段时间，每小时有次数上限，攒着的事件合并成一次分析；定时分析开始时会一并覆盖已排队的事件。需要 AI 交易开启且机器人在运行。
            </p>
            <label className="toggle" style={{ marginTop: 12 }}>
              <input type="checkbox" checked={hk.enabled} onChange={(e) => setHk({ ...hk, enabled: e.target.checked })} />
              <span>启用事件触发</span>
            </label>
            <label className="toggle" style={{ marginTop: 12 }}>
              <input type="checkbox" checked={hk.adverse_move} disabled={!hk.enabled} onChange={(e) => setHk({ ...hk, adverse_move: e.target.checked })} />
              <span>持仓浮亏走到止损距离的 60%</span>
            </label>
            <label className="toggle" style={{ marginTop: 12 }}>
              <input type="checkbox" checked={hk.profit_milestone} disabled={!hk.enabled} onChange={(e) => setHk({ ...hk, profit_milestone: e.target.checked })} />
              <span>持仓浮盈走出一个止损距离（追踪止损起点）</span>
            </label>
            <label className="toggle" style={{ marginTop: 12 }}>
              <input type="checkbox" checked={hk.position_closed} disabled={!hk.enabled} onChange={(e) => setHk({ ...hk, position_closed: e.target.checked })} />
              <span>仓位被交易所止损或止盈平掉</span>
            </label>
            <div className="ai-fields">
              <div>
                <label className="f" htmlFor="hkMove">
                  币价一小时内波动达到 %（0 = 不启用）
                </label>
                <input id="hkMove" type="number" step={0.5} min={0} max={20} value={hk.big_move_pct} disabled={!hk.enabled} onChange={(e) => setHk({ ...hk, big_move_pct: +e.target.value })} />
                <p className="hint">只看交易策略页里选的币，同一个币两小时内最多触发一次。</p>
              </div>
              <div>
                <label className="f" htmlFor="hkGap">
                  两次分析至少间隔（分钟）
                </label>
                <input id="hkGap" type="number" step={1} min={1} max={120} value={hk.min_gap_minutes} disabled={!hk.enabled} onChange={(e) => setHk({ ...hk, min_gap_minutes: +e.target.value })} />
              </div>
              <div>
                <label className="f" htmlFor="hkMax">
                  事件触发的分析每小时最多（次）
                </label>
                <input id="hkMax" type="number" step={1} min={1} max={60} value={hk.max_per_hour} disabled={!hk.enabled} onChange={(e) => setHk({ ...hk, max_per_hour: +e.target.value })} />
              </div>
            </div>
            <div className="row">
              <button onClick={saveHooks} disabled={busy === "hooks"}>
                {busy === "hooks" ? "保存中…" : "保存"}
              </button>
            </div>

            <label className="f" style={{ marginTop: 20 }}>外部 Webhook</label>
            <p className="hint">
              让 TradingView 等外部系统在需要时叫醒 AI。生成后得到一个带密钥的地址，对它发 POST 请求即可，内容是纯文本或 JSON 的 message 字段。这条消息只会作为不可信的线索交给 AI，不能下单，也不能改任何限制；每小时最多 30 次，猜错密钥会被限速。
            </p>
            {newToken ? (
              <div className="msg warn" style={{ wordBreak: "break-all" }}>
                {`${typeof location !== "undefined" ? location.origin : ""}/api/hook/${newToken}`}
                <div className="muted">只显示这一次。示例：curl -X POST -d "BTC 突破前高" 上面的地址</div>
              </div>
            ) : hk.webhook_set ? (
              <p className="hint">已启用，密钥 {hk.webhook_token}（完整地址不再显示，忘了可以重新生成）。</p>
            ) : (
              <p className="hint">未启用。</p>
            )}
            <div className="row">
              <button className="ghost" onClick={makeToken} disabled={busy === "token"}>
                {hk.webhook_set ? "重新生成地址" : "生成 Webhook 地址"}
              </button>
              {hk.webhook_set && (
                <button className="ghost" onClick={dropToken} disabled={busy === "token"}>
                  停用
                </button>
              )}
            </div>
          </>
        )}
      </Card>

      <Card
        title="决策记录"
        note="最近 20 次，最新在最上面"
        right={
          <button className="ghost sm" onClick={() => load()} disabled={busy === "load"}>
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
