"use client";
import { useCallback, useEffect, useRef, useState } from "react";
import { useFeedback } from "@/components/Feedback";
import { api, del, post } from "@/lib/api";
import { AI_ACT, AI_SOURCE, AI_STATUS, base, utc } from "@/lib/fmt";
import { Card, Empty, MsgBox, Skeleton, StateLabel, useAction } from "@/lib/ui";

type Channel = {
  id: string;
  name: string;
  provider: string;
  api_protocol: string;
  reasoning_effort: string;
  claude_thinking: string;
  claude_effort: string;
  claude_thinking_budget: number;
  stream: boolean;
  model: string;
  base_url: string;
  api_key: string | null;
};

type Cfg = {
  channels: Channel[];
  active_channel: string;
  provider: string;
  api_protocol: string;
  reasoning_effort: string;
  claude_thinking: string;
  claude_effort: string;
  claude_thinking_budget: number;
  stream: boolean;
  model: string;
  base_url: string;
  api_key: string | null;
  interval: number;
  delay_seconds: number;
  min_confidence: number;
  max_daily_loss_pct: number;
  max_drawdown_pct: number;
  max_consecutive_losses: number;
  shadow: boolean;
  enabled: boolean;
};

type AiData = {
  config: Cfg;
  state: { running: boolean; last_run: string | null; next_run: string | null; last_error: string | null };
  position_count: number;
  max_open: number;
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
        <span>{r.channel ? `${r.channel} · ` : ""}{r.model}</span>
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
  const [editId, setEditId] = useState("");
  const editRef = useRef("");
  const [chName, setChName] = useState("");
  const [newName, setNewName] = useState("");
  const [provider, setProvider] = useState("anthropic");
  const [protocol, setProtocol] = useState("chat_completions");
  const [effort, setEffort] = useState("default");
  const [claudeThinking, setClaudeThinking] = useState("default");
  const [claudeEffort, setClaudeEffort] = useState("default");
  const [claudeBudget, setClaudeBudget] = useState(4096);
  const [stream, setStream] = useState(false);
  const [model, setModel] = useState(DEFAULT_MODEL.anthropic);
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [freq, setFreq] = useState(240);
  const [delay, setDelay] = useState(180);
  const [conf, setConf] = useState(0.6);
  const [loss, setLoss] = useState(5);
  const [drawdown, setDrawdown] = useState(20);
  const [streak, setStreak] = useState(5);
  const [shadow, setShadow] = useState(false);

  const selectEdit = useCallback((id: string) => {
    editRef.current = id;
    setEditId(id);
  }, []);

  // The model fields are those of the channel being edited; the rest is shared by all channels.
  const fill = useCallback((c: Cfg, id?: string) => {
    const ch: Channel | Cfg = c.channels?.find((x) => x.id === id) ?? c.channels?.find((x) => x.id === c.active_channel) ?? c;
    setChName((ch as Channel).name || "");
    setProvider(ch.provider);
    setProtocol(ch.api_protocol || "chat_completions");
    setEffort(ch.reasoning_effort || "default");
    setClaudeThinking(ch.claude_thinking || "default");
    setClaudeEffort(ch.claude_effort || "default");
    setClaudeBudget(ch.claude_thinking_budget ?? 4096);
    setStream(!!ch.stream);
    setModel(ch.model);
    setBaseUrl(ch.base_url || "");
    setFreq(c.interval);
    setDelay(c.delay_seconds ?? 180);
    setConf(c.min_confidence);
    setLoss(c.max_daily_loss_pct);
    setDrawdown(c.max_drawdown_pct ?? 20);
    setStreak(c.max_consecutive_losses ?? 5);
    setShadow(!!c.shadow);
  }, []);

  const load = useCallback(async (prefer?: string) => {
    try {
      const r = await api<AiData>("/api/ai");
      setData(r);
      const want = prefer || editRef.current;
      const id = r.config.channels?.some((x) => x.id === want) ? want : r.config.active_channel;
      selectEdit(id);
      fill(r.config, id);
      setErr("");
    } catch (e: any) {
      setErr(e?.message || "加载失败");
    }
  }, [fill, selectEdit]);

  useEffect(() => {
    load();
  }, [load]);

  // Refresh decision state without replacing configuration the user is editing.
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
  const edit = c?.channels?.find((x) => x.id === editId);
  const isActive = !!c && editId === c.active_channel;
  const on = !!c?.enabled;
  const st = data?.state;

  function switchProvider(p: string) {
    if (p === provider) return;
    setProvider(p);
    setModel(DEFAULT_MODEL[p] || "");
    setBaseUrl("");
  }

  function pickChannel(id: string) {
    if (!c) return;
    selectEdit(id);
    fill(c, id);
    setApiKey("");
  }

  async function addChannel() {
    await run("channel", async () => {
      const r = await post<any>("/api/ai/channels", { name: newName, provider });
      setNewName("");
      await load(r.created);
      return { kind: "good", text: "已新增渠道，填好接口地址和 API Key 并保存后，再选择“设为使用”。" };
    });
  }

  async function useChannel() {
    if (!c || !edit) return;
    if (on) {
      const ok = await confirm({
        title: `改用「${edit.name}」？`,
        body: "下一轮分析起使用这个渠道的模型和 Key。正在进行的分析不会被打断。",
        ok: "改用",
      });
      if (!ok) return;
    }
    await run("channel", async () => {
      await post("/api/ai/channels/select", { id: editId });
      await load(editId);
      return { kind: "good", text: `已改用「${edit.name}」。` };
    });
  }

  async function removeChannel() {
    if (!c || !edit) return;
    const ok = await confirm({ title: `删除渠道「${edit.name}」？`, body: "渠道里保存的 API Key 会一并删除。", ok: "删除", danger: true });
    if (!ok) return;
    await run("channel", async () => {
      await del(`/api/ai/channels/${editId}`);
      await load(c.active_channel);
      return { kind: "good", text: "已删除渠道。" };
    });
  }

  async function saveCfg() {
    await run("save", async () => {
      if (!Number.isInteger(freq) || freq < 1 || freq > 1440) throw new Error("分析间隔须为 1 到 1440 分钟");
      if (!Number.isInteger(delay) || delay < 0 || delay >= freq * 60) throw new Error("延迟须为非负整数秒，且小于分析间隔");
      if (provider === "anthropic" && claudeThinking === "enabled" && (!Number.isInteger(claudeBudget) || claudeBudget < 1024 || claudeBudget >= 16000)) throw new Error("Claude 手动思考预算须为 1024–15999 tokens");
      if (provider === "anthropic" && claudeThinking === "disabled" && ["xhigh", "max"].includes(claudeEffort)) throw new Error("xhigh/max 等级下不能关闭思考，请选择自适应模式或降低等级");
      await post("/api/ai/config", {
        channel: editId,
        name: chName,
        provider,
        api_protocol: protocol,
        reasoning_effort: effort,
        claude_thinking: claudeThinking,
        claude_effort: claudeEffort,
        claude_thinking_budget: claudeBudget,
        stream,
        model,
        base_url: baseUrl,
        api_key: apiKey || null,
        interval: freq,
        delay_seconds: delay,
        min_confidence: conf,
        max_daily_loss_pct: loss,
        max_drawdown_pct: drawdown,
        max_consecutive_losses: streak,
        shadow,
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
            开启后，<b>所有自动开平仓都由大模型决定</b>，按设定间隔分析行情和持仓。止损（2%–10%）和止盈由大模型设置，之后只能收紧止损、不能放宽；两次分析之间系统按止损距离的一半自动追踪止损，作为保底；每笔仓位和杠杆也由大模型决定，系统只设上限：风险不超过交易配置里的“每笔最多亏”，杠杆不超过最高杠杆、交易所上限，并保证止损先于强平。
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
          </div>
          <p className="hint">与交易配置页共用机器人持仓上限，不限制每日开仓次数。平仓后释放名额，AI 可按信号继续开多或开空（需允许做空）。</p>
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
          </div>
          <MsgBox m={msg} />
          {st?.last_error && !st.running ? <div className="msg warn">上次出错：{st.last_error}</div> : null}
        </Card>

        <Card
          title="大模型与风控"
          className="ai-settings"
          right={<StateLabel on={!!edit?.api_key}>{edit?.api_key ? `已保存 ${edit.api_key}` : "未设置"}</StateLabel>}
        >
          {!c ? (
            <Skeleton lines={6} />
          ) : (
            <>
              <div className="ai-channels">
                <label className="f" htmlFor="aiChannel">API 渠道</label>
                <div className="ai-channel-row">
                  <select id="aiChannel" value={editId} onChange={(e) => pickChannel(e.target.value)}>
                    {c.channels.map((ch) => (
                      <option key={ch.id} value={ch.id}>
                        {ch.name}
                        {ch.id === c.active_channel ? "（使用中）" : ""}
                        {ch.api_key ? "" : "（未设置 Key）"}
                      </option>
                    ))}
                  </select>
                  {!isActive && (
                    <button className="ghost" onClick={useChannel} disabled={busy === "channel" || !edit?.api_key}>
                      设为使用
                    </button>
                  )}
                  {!isActive && (
                    <button className="ghost" onClick={removeChannel} disabled={busy === "channel"}>
                      删除
                    </button>
                  )}
                </div>
                <div className="ai-channel-row">
                  <input value={newName} maxLength={40} placeholder="新渠道名称，例如：备用中转站" onChange={(e) => setNewName(e.target.value)} />
                  <button className="ghost" onClick={addChannel} disabled={busy === "channel" || !newName.trim()}>
                    新增渠道
                  </button>
                </div>
                <p className="hint">下面的接口、模型和 Key 属于当前选中的渠道；间隔、风控等设置所有渠道共用。分析只用“使用中”的渠道，不会自动切换。</p>
                <label className="f" htmlFor="aiChannelName">渠道名称</label>
                <input id="aiChannelName" value={chName} maxLength={40} onChange={(e) => setChName(e.target.value)} />
              </div>
              <div className="seg ai-provider" role="radiogroup" aria-label="接口">
                <button role="radio" aria-checked={provider === "anthropic"} className={provider === "anthropic" ? "on" : ""} onClick={() => switchProvider("anthropic")}>
                  Claude
                </button>
                <button role="radio" aria-checked={provider === "openai"} className={provider === "openai" ? "on" : ""} onClick={() => switchProvider("openai")}>
                  OpenAI 兼容
                </button>
              </div>
              {provider === "openai" ? (
                <div className="ai-fields ai-model-options">
                  <div>
                    <label className="f" htmlFor="aiProtocol">接口类型</label>
                    <select id="aiProtocol" value={protocol} onChange={(e) => setProtocol(e.target.value)}>
                      <option value="chat_completions">Chat Completions（兼容模式）</option>
                      <option value="responses">Responses</option>
                    </select>
                    <p className="hint">按服务商支持的类型选择。原有配置默认使用 Chat Completions。</p>
                  </div>
                  <div>
                    <label className="f" htmlFor="aiEffort">思考等级</label>
                    <select id="aiEffort" value={effort} onChange={(e) => setEffort(e.target.value)}>
                      <option value="default">服务商默认（不传参数）</option>
                      <option value="none">不启用（none）</option>
                      <option value="minimal">极低（minimal）</option>
                      <option value="low">低（low）</option>
                      <option value="medium">中（medium）</option>
                      <option value="high">高（high）</option>
                      <option value="xhigh">极高（xhigh）</option>
                      <option value="max">最高（max）</option>
                    </select>
                    <p className="hint">可用等级取决于模型和服务商。不确定时选默认；更高等级可能增加耗时与费用。</p>
                  </div>
                </div>
              ) : (
                <div className="ai-fields ai-model-options">
                  <div>
                    <label className="f" htmlFor="claudeThinking">Claude 思考模式</label>
                    <select id="claudeThinking" value={claudeThinking} onChange={(e) => setClaudeThinking(e.target.value)}>
                      <option value="default">服务商默认（不传参数）</option>
                      <option value="adaptive">自适应思考（新版模型）</option>
                      <option value="enabled">手动预算（旧版模型）</option>
                      <option value="disabled">关闭思考（模型需支持）</option>
                    </select>
                    <p className="hint">Opus 5.5 等新版模型默认已启用自适应思考，不支持手动预算或关闭。具体以模型和服务商支持为准。</p>
                  </div>
                  <div>
                    <label className="f" htmlFor="claudeEffort">Claude 思考等级</label>
                    <select id="claudeEffort" value={claudeEffort} onChange={(e) => setClaudeEffort(e.target.value)}>
                      <option value="default">服务商默认（不传参数）</option>
                      <option value="low">低（low）</option>
                      <option value="medium">中（medium）</option>
                      <option value="high">高（high）</option>
                      <option value="xhigh">极高（xhigh）</option>
                      <option value="max">最高（max）</option>
                    </select>
                    <p className="hint">等级支持因模型而异，更高等级可能增加耗时与费用。不确定时选默认。</p>
                  </div>
                  {claudeThinking === "enabled" && (
                    <div className="ai-budget">
                      <label className="f" htmlFor="claudeBudget">手动思考预算（tokens）</label>
                      <input id="claudeBudget" type="number" min={1024} max={15999} step={1} value={claudeBudget} onChange={(e) => setClaudeBudget(+e.target.value)} />
                      <p className="hint">范围 1024–15999；与最终回答共用总输出上限（非流式 32000，流式 64000 tokens）。</p>
                    </div>
                  )}
                </div>
              )}
              <label className="toggle" style={{ marginTop: 12 }}>
                <input type="checkbox" checked={stream} onChange={(e) => setStream(e.target.checked)} />
                <span>流式输出</span>
              </label>
              <p className="hint">
                模型边生成边传回，长时间思考时连接不会被中转站当作无响应断开，适合高思考等级。{provider === "anthropic" ? "开启后 Claude 输出上限从 32000 提高到 64000 tokens。" : ""}收齐完整结果后才会执行交易；中转站不支持流式时请关闭。
              </p>
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
              <p className="hint">填写根地址会自动补上 /v1，也可填写带 /v1 的基础地址或完整接口地址。返回网页时会提示实际请求路径。</p>
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
                <div>
                  <label className="f" htmlFor="aiConf">
                    最低置信度（低于不执行）
                  </label>
                  <input id="aiConf" type="number" step={0.05} min={0} max={1} value={conf} onChange={(e) => setConf(+e.target.value)} />
                </div>
                <div>
                  <label className="f" htmlFor="aiLoss">
                    当日亏损达到 % 停止开仓
                  </label>
                  <input id="aiLoss" type="number" step={0.5} min={0.5} max={50} value={loss} onChange={(e) => setLoss(+e.target.value)} />
                </div>
                <div>
                  <label className="f" htmlFor="aiDrawdown">
                    账户回撤达到 % 暂停机器人（0 = 不限）
                  </label>
                  <input id="aiDrawdown" type="number" step={1} min={0} max={90} value={drawdown} onChange={(e) => setDrawdown(+e.target.value)} />
                </div>
                <div>
                  <label className="f" htmlFor="aiStreak">
                    连续亏损 N 笔停止开仓（0 = 不限）
                  </label>
                  <input id="aiStreak" type="number" step={1} min={0} max={50} value={streak} onChange={(e) => setStreak(+e.target.value)} />
                </div>
              </div>
              <label className="toggle" style={{ marginTop: 12 }}>
                <input type="checkbox" checked={shadow} onChange={(e) => setShadow(e.target.checked)} />
                <span>影子模式</span>
              </label>
              <p className="hint">
                开启后按计划分析并记录决策，但不下任何单，用来观察模型质量。回撤从机器人每次启动时的权益算起，触发后自动暂停；连续亏损只统计启动之后平掉的单，重新启动即可恢复。
              </p>
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
