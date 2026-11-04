"use client";
import { useState } from "react";
import { useFeedback } from "@/components/Feedback";
import { del, post } from "@/lib/api";
import { useOv } from "@/lib/store";
import { Card, MsgBox, StateLabel, useAction } from "@/lib/ui";

export default function NotifyPage() {
  const { d } = useOv();
  const { confirm } = useFeedback();
  const [token, setToken] = useState("");
  const { busy, msg, run } = useAction();
  const on = !!d.telegram;

  async function save() {
    await run("save", async () => {
      const r = await post<any>("/api/telegram", { token });
      setToken("");
      return { kind: "good", text: `已连接 @${r.bot}，手机上应该收到了一条测试消息。` };
    });
  }

  async function test() {
    await run("test", async () => {
      await post("/api/telegram/test");
      return { kind: "good", text: "已发送，看看手机。" };
    });
  }

  async function off() {
    const ok = await confirm({ title: "关闭 Telegram 通知？", body: "关闭后开仓、平仓不再推送到手机。", ok: "关闭", danger: true });
    if (!ok) return;
    await run("del", async () => {
      await del("/api/telegram");
      return { kind: "good", text: "已关闭通知。" };
    });
  }

  return (
    <Card
      title="Telegram 通知"
      right={<StateLabel on={on}>{on ? "已连接" : "未设置"}</StateLabel>}
    >
      <ol className="steps">
        <li>
          Telegram 搜 <b>@BotFather</b>，发 <code>/newbot</code>，起个名字
        </li>
        <li>
          打开它给你的 Bot 链接，<b>点 Start</b>
        </li>
        <li>把 BotFather 给的 Token 贴到下面</li>
      </ol>
      <label className="f" htmlFor="token">
        Bot Token
      </label>
      <input
        id="token"
        type="password"
        value={token}
        placeholder="123456789:AAH..."
        autoComplete="new-password"
        onChange={(e) => setToken(e.target.value)}
      />
      <div className="row">
        <button onClick={save} disabled={busy === "save" || !token}>
          {busy === "save" ? "连接中…" : "连接"}
        </button>
        {on && (
          <button className="ghost" onClick={test} disabled={busy === "test"}>
            {busy === "test" ? "发送中…" : "发测试消息"}
          </button>
        )}
        {on && (
          <button className="ghost" onClick={off} disabled={busy === "del"}>
            关闭通知
          </button>
        )}
      </div>
      <MsgBox m={msg} />
      <p className="hint" style={{ marginTop: 14 }}>
        推送开仓成交、平仓成交（带盈亏）、订单取消和警告；机器人启动和状态变化静音。
      </p>
    </Card>
  );
}
