"use client";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { useEffect, useMemo, useState } from "react";
import { post } from "@/lib/api";
import { useOv } from "@/lib/store";
import { useFeedback } from "./Feedback";
import {
  IconAccount,
  IconAi,
  IconLogs,
  IconMenu,
  IconMore,
  IconKnowledge,
  IconNotify,
  IconOverview,
  IconPositions,
  IconRefresh,
  IconStrategy,
  IconTrades,
} from "./icons";

type Item = { href: string; label: string; icon: () => React.ReactElement };

const TRADE: Item[] = [
  { href: "/", label: "概览", icon: IconOverview },
  { href: "/positions/", label: "持仓", icon: IconPositions },
  { href: "/trades/", label: "交易记录", icon: IconTrades },
];

const SETUP: Item[] = [
  { href: "/strategy/", label: "交易配置", icon: IconStrategy },
  { href: "/ai/", label: "AI 交易", icon: IconAi },
  { href: "/knowledge/", label: "知识库", icon: IconKnowledge },
  { href: "/account/", label: "账户与资金", icon: IconAccount },
  { href: "/notify/", label: "通知", icon: IconNotify },
  { href: "/logs/", label: "运行日志", icon: IconLogs },
];

const ALL = [...TRADE, ...SETUP];

const STATE_TEXT: Record<string, string> = { running: "运行中", stopped: "已暂停", offline: "机器人离线" };

const norm = (p: string) => p.replace(/\/+$/, "") || "/";

export function Shell({ children }: { children: React.ReactNode }) {
  const pathname = usePathname();
  const path = norm(pathname || "/");
  const { d, loaded, offline, updated, refresh } = useOv();
  const { confirm, toast } = useFeedback();
  const [open, setOpen] = useState(false);
  const [botBusy, setBotBusy] = useState(false);

  const current = useMemo(() => ALL.find((x) => norm(x.href) === path) ?? ALL[0], [path]);

  useEffect(() => setOpen(false), [pathname]);

  useEffect(() => {
    document.title = `${current.label} · Tide`;
  }, [current.label]);

  const fut = d.market === "futures";
  const running = d.state === "running";
  const stopped = d.state === "stopped";
  const openCount = (d.open || []).length;

  async function bot(action: "start" | "stop") {
    if (action === "stop") {
      const ok = await confirm({
        title: "暂停机器人？",
        body: "暂停后不会再自动开仓。已有仓位的止损和移动止盈继续执行，仍可手动平仓。",
        ok: "暂停",
        danger: true,
      });
      if (!ok) return;
    }
    setBotBusy(true);
    try {
      await post(`/api/bot/${action}`);
      toast(action === "start" ? "机器人已启动" : "机器人已暂停");
      await refresh();
    } catch (e: any) {
      toast(e?.message || "操作失败", "err");
    } finally {
      setBotBusy(false);
    }
  }

  const navLink = (it: Item) => {
    const on = norm(it.href) === path;
    const badge =
      it.href === "/positions/" ? (openCount || 0) : it.href === "/ai/" && d.ai ? 1 : 0;
    return (
      <Link key={it.href} href={it.href} className={"nav-a" + (on ? " on" : "")} aria-current={on ? "page" : undefined}>
        <it.icon />
        {it.label}
        {badge > 0 && <span className="badge">{it.href === "/ai/" ? "开" : badge}</span>}
      </Link>
    );
  };

  return (
    <div className="shell">
      <aside className={open ? "open" : ""} id="side">
        <div className="logo">
          <svg width="30" height="30" viewBox="0 0 30 30" aria-hidden="true">
            <rect width="30" height="30" rx="8" fill="#e0950a" />
            <path
              d="M6 20l5-6 4 3 8-9"
              fill="none"
              stroke="#0f1722"
              strokeWidth="2.6"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
          <div>
            <b>Tide</b>
            <small>{d.server_ip || "管理后台"}</small>
          </div>
        </div>

        <nav>
          {TRADE.map(navLink)}
          <div className="sep" />
          {SETUP.map(navLink)}
        </nav>

        <div className="side-foot">
          <div className="row2">
            <span className={"dot " + (d.state || "")} aria-live="polite">
              {STATE_TEXT[d.state] || (loaded ? "连接中" : "连接中…")}
            </span>
          </div>
          <div>{loaded ? `实盘 · ${fut ? `合约 ${d.leverage}×` : "现货"} · ${(d.pairs || []).length} 个币` : "—"}</div>
          <div className="side-time">{updated ? `更新于 ${new Date(updated).toLocaleTimeString("zh-CN", { hour12: false })}` : " "}</div>
        </div>
      </aside>

      {open && <div className="drawer-scrim" onClick={() => setOpen(false)} aria-hidden="true" />}

      <main>
        <div className="bar">
          <div className="bar-in">
            <button className="ghost sm menu-btn" onClick={() => setOpen(true)} aria-label="打开菜单">
              <IconMenu />
            </button>
            <h1>{current.label}</h1>
            <span className="sp" />
            <span className={"pill mk" + (fut ? " fut" : "")}>
              {fut ? `合约 ${d.leverage}×` : "现货"}
              {fut && d.short ? <span className="hide-sm"> 多空</span> : null}
            </span>
            <span className="pill live">实盘</span>
            <button className="ghost sm" onClick={async () => {
              try { await post("/api/auth/logout"); location.replace("/login/"); }
              catch { toast("退出失败，请重试", "err"); }
            }}>退出登录</button>
            {stopped && (
              <button className="sm" disabled={botBusy} onClick={() => bot("start")}>
                启动
              </button>
            )}
            {running && (
              <button className="ghost sm" disabled={botBusy} onClick={() => bot("stop")}>
                暂停
              </button>
            )}
            <button
              className={"ghost sm icon-btn" + (botBusy ? " spin" : "")}
              onClick={() => refresh()}
              aria-label="刷新数据"
              title="刷新数据"
            >
              <IconRefresh />
            </button>
          </div>
        </div>

        <div className={"content " + (path === "/" ? "overview-content" : `workspace-content page-${path.slice(1).replace(/[^a-z-]/g, "")}`)}>
          {offline && (
            <div className="msg warn banner" role="status">
              {offline}
            </div>
          )}
          {children}
        </div>
      </main>

      <div className="tabbar" role="navigation" aria-label="主要页面">
        {TRADE.filter((x) => x.href !== "/trades/").map((it) => {
          const on = norm(it.href) === path;
          return (
            <Link key={it.href} href={it.href} className={"tab" + (on ? " on" : "")}>
              <it.icon />
              {it.label}
              {it.href === "/positions/" && openCount > 0 && <i className="dot-badge">{openCount}</i>}
            </Link>
          );
        })}
        <Link href="/trades/" className={"tab" + (path === "/trades" ? " on" : "")}>
          <IconTrades />
          记录
        </Link>
        <button className={"tab" + (SETUP.some((s) => norm(s.href) === path) ? " on" : "")} onClick={() => setOpen(true)}>
          <IconMore />
          更多
        </button>
      </div>
    </div>
  );
}
