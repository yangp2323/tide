"use client";
import { useState } from "react";
import "./style.css";
export default function Login() {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [visible, setVisible] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function submit(event: React.FormEvent) {
    event.preventDefault(); setBusy(true); setError("");
    try {
      const response = await fetch("/api/auth/login", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ username, password }) });
      const data = await response.json();
      if (!response.ok) throw new Error(data.detail || "登录失败，请重试");
      const next = new URLSearchParams(location.search).get("next") || "/";
      const target = new URL(next, location.origin);
      location.replace(target.origin === location.origin && !target.pathname.startsWith("/login") ? target.href : "/");
    } catch (e: any) { setError(e.message || "暂时无法连接，请稍后重试"); setBusy(false); }
  }
  return <div className="login-screen" data-runtime="next-dev">
    <section className="login-story" aria-label="Tide 量化交易管理">
      <a className="login-brand" href="/login"><span className="login-mark">↗</span>Tide<span className="login-tag">CONSOLE</span></a>
      <div className="login-story-body"><div className="login-eyebrow"><span /> YOUR TRADING WORKSPACE</div><h1>让策略运行。<br />让决策有据。</h1><p>持仓、策略与资金，尽在一个工作台。<br />登录后，继续掌握你的交易节奏。</p>
      <div className="login-chart" aria-hidden="true"><div className="chart-grid" /><svg viewBox="0 0 500 170" fill="none"><defs><linearGradient id="area" x1="0" y1="0" x2="0" y2="1"><stop stopColor="#e0950a" stopOpacity=".2"/><stop offset="1" stopColor="#e0950a" stopOpacity="0"/></linearGradient></defs><path d="M0 141 L35 129 65 138 100 103 132 111 165 85 196 97 230 59 263 77 295 44 328 58 363 30 395 42 430 12 462 23 500 0 V170 H0Z" fill="url(#area)"/><path d="M0 141 L35 129 65 138 100 103 132 111 165 85 196 97 230 59 263 77 295 44 328 58 363 30 395 42 430 12 462 23 500 0" stroke="#e7aa35" strokeWidth="2.5"/></svg><span>策略 · 执行 · 洞察</span></div></div>
      <div className="login-story-foot">TIDE / 管理控制台<span>PRIVATE ACCESS</span></div>
    </section>
    <main className="login-main"><div className="login-card"><div className="login-eyebrow">WELCOME BACK</div><h2>登录管理后台</h2><p className="login-intro">使用管理员账号，进入你的交易工作台。</p>
      <form onSubmit={submit}><label htmlFor="username">账号</label><input id="username" name="username" autoComplete="username" placeholder="请输入管理员账号" required autoFocus maxLength={256} value={username} onChange={e => setUsername(e.target.value)} disabled={busy} />
      <label htmlFor="password">密码</label><div className="login-password"><input id="password" name="password" type={visible ? "text" : "password"} autoComplete="current-password" placeholder="请输入密码" required maxLength={1024} value={password} onChange={e => setPassword(e.target.value)} disabled={busy}/><button type="button" onClick={() => setVisible(!visible)} aria-label={visible ? "隐藏密码" : "显示密码"}>{visible ? "隐藏" : "显示"}</button></div>
      <div className="login-error" role="alert" aria-live="polite">{error}</div><button className="login-submit" type="submit" disabled={busy}>{busy ? "正在登录…" : "登录工作台"}<span aria-hidden="true">→</span></button></form>
      <div className="login-note"><span aria-hidden="true">◇</span> 仅限授权访问 · 登录状态保留 12 小时</div></div><footer>Tide · 量化交易管理</footer></main>
  </div>;
}
