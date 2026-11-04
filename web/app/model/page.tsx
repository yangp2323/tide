"use client";
import { useCallback, useEffect, useRef, useState } from "react";
import { useFeedback } from "@/components/Feedback";
import { api, del, post } from "@/lib/api";
import { AiData, Cfg, Channel, DEFAULT_MODEL } from "@/lib/ai";
import { Card, Empty, MsgBox, Skeleton, StateLabel, useAction } from "@/lib/ui";

export default function ModelPage() {
  const { confirm } = useFeedback();
  const { busy, msg, run } = useAction();
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

  const selectEdit = useCallback((id: string) => {
    editRef.current = id;
    setEditId(id);
  }, []);

  // The fields on this page are those of the channel being edited.
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
  }, []);

  const load = useCallback(
    async (prefer?: string) => {
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
    },
    [fill, selectEdit],
  );

  useEffect(() => {
    load();
  }, [load]);

  const c = data?.config;
  const edit = c?.channels?.find((x) => x.id === editId);
  const isActive = !!c && editId === c.active_channel;
  const on = !!c?.enabled;

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
      });
      setApiKey("");
      await load();
      return { kind: "good", text: "已保存。" };
    });
  }

  return (
    <Card
      title="大模型"
      note="API 渠道、模型和 Key"
      className="ai-settings"
      right={<StateLabel on={!!edit?.api_key}>{edit?.api_key ? `已保存 ${edit.api_key}` : "未设置"}</StateLabel>}
    >
      {err ? (
        <Empty>{err}</Empty>
      ) : !c ? (
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
                <p className="hint">下面的接口、模型和 Key 属于当前选中的渠道；分析间隔、风控等设置所有渠道共用，在「交易策略」页设置。分析只用“使用中”的渠道，不会自动切换。</p>
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
              <div className="row">
                <button onClick={saveCfg} disabled={busy === "save"}>
                  {busy === "save" ? "保存中…" : "保存"}
                </button>
              </div>
              <MsgBox m={msg} />
            </>
      )}
    </Card>
  );
}
