"use client";
import { useState } from "react";
import { post } from "@/lib/api";
import { money } from "@/lib/fmt";
import { useOv } from "@/lib/store";
import { Card, MsgBox, Skeleton, StateLabel, useAction } from "@/lib/ui";

export default function AccountPage() {
  const { d, loaded } = useOv();
  const [key, setKey] = useState("");
  const [secret, setSecret] = useState("");
  const [capital, setCapital] = useState("");

  const keyAct = useAction();
  const capAct = useAction();

  const fut = d.market === "futures";
  const saved = !!d.key;

  async function saveKey() {
    await keyAct.run("save", async () => {
      const r = await post<any>("/api/key", { key, secret });
      setKey("");
      setSecret("");
      const bal =
        Object.entries(r.balance || {})
          .map(([c, v]: any) => `${c} ${(+v).toFixed(6)}`)
          .join("，") || "空";
      return {
        kind: r.ip_bound ? "good" : "warn",
        text:
          `验证通过，已保存。\n余额：${bal}` +
          (r.ip_bound ? "" : `\n这个 Key 没绑定 IP，请在 Bybit 把 IP 白名单设为 ${d.server_ip || "服务器 IP"}`),
      };
    });
  }

  async function saveCapital() {
    await capAct.run("save", async () => {
      const n = parseInt(capital, 10) || null;
      await post("/api/capital", { capital: n });
      setCapital("");
      return { kind: "good", text: n ? `已保存：最多动用 ${n} USDT。` : "已保存：使用账户里全部 USDT。" };
    });
  }

  return (
    <div className="grid g2">
      <Card
        title="Bybit API Key"
        right={<StateLabel on={saved}>{saved ? `已保存 ${d.key}` : "未设置"}</StateLabel>}
      >
        <ol className="steps">
          <li>Bybit → 头像 → API → 创建新密钥 → 系统生成</li>
          <li>
            {fut ? (
              <>
                权限勾选 <b>合约 - 订单</b> 和 <b>合约 - 持仓</b>，<b>不要勾提现</b>
              </>
            ) : (
              <>
                权限勾选 <b>现货 - 交易</b>，<b>不要勾提现</b>
              </>
            )}
          </li>
          <li>
            IP 白名单填 <code>{d.server_ip || "—"}</code>
          </li>
        </ol>
        <label className="f" htmlFor="key">
          API Key
        </label>
        <input id="key" value={key} onChange={(e) => setKey(e.target.value)} autoComplete="off" spellCheck={false} />
        <label className="f" htmlFor="secret">
          API Secret
        </label>
        <input
          id="secret"
          type="password"
          value={secret}
          onChange={(e) => setSecret(e.target.value)}
          autoComplete="new-password"
        />
        <div className="row">
          <button onClick={saveKey} disabled={keyAct.busy === "save" || !key || !secret}>
            {keyAct.busy === "save" ? "验证中…" : "验证并保存"}
          </button>
        </div>
        <MsgBox m={keyAct.msg} />
      </Card>

      <Card
        className="live-on"
        title="实盘资金"
        right={<span className="pill live">实盘 · 真钱</span>}
      >
        {!loaded ? (
          <Skeleton lines={3} />
        ) : (
          <>
            <p className="hint">
              机器人用你 Bybit 账户里的真钱自动交易，可能亏损。当前最多动用 <b>{d.capital ? d.capital + " USDT" : "账户里全部"}</b>。
            </p>
            <label className="f" htmlFor="capital">
              机器人最多可动用多少 USDT（选填，不填就用账户里全部 USDT）
            </label>
            <input
              id="capital"
              type="number"
              inputMode="numeric"
              step={10}
              min={d.min_capital}
              value={capital}
              placeholder="不填 = 全部余额"
              onChange={(e) => setCapital(e.target.value)}
              style={{ maxWidth: 240 }}
            />
            <p className="hint" style={{ marginTop: 8 }}>
              同时最多 {d.max_open} 笔，每笔至少 {d.min_stake} USDT{fut ? " 保证金" : ""}，所以账户（或填的金额）至少要{" "}
              {d.min_capital} USDT。
              {fut ? `合约 ${d.leverage}× 下，每笔实际仓位是保证金的 ${d.leverage} 倍。` : ""}
            </p>
            <div className="row">
              <button onClick={saveCapital} disabled={capAct.busy === "save"}>
                {capAct.busy === "save" ? "保存中…" : "保存资金上限"}
              </button>
            </div>
            <MsgBox m={capAct.msg} />
            <p className="hint" style={{ marginTop: 10 }}>
              当前资金上限：{d.capital ? `${money(d.capital, 0)} USDT` : "不限制（账户全部余额）"}
            </p>
          </>
        )}
      </Card>
    </div>
  );
}
