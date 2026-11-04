"use client";
import { createContext, useCallback, useContext, useEffect, useRef, useState } from "react";
import { api } from "./api";

export type Ov = Record<string, any>;
export const POLL_MS = 15000;

type Ctx = {
  d: Ov;
  loaded: boolean;
  offline: string;
  refreshing: boolean;
  /** 最近一次成功刷新的时间戳（毫秒），用于显示"更新于 …" */
  updated: number | null;
  refresh: () => Promise<void>;
};

const C = createContext<Ctx>({ d: {}, loaded: false, offline: "", refreshing: false, updated: null, refresh: async () => {} });

export const useOv = () => useContext(C);

/** 概览数据（服务器 IP、市场、持仓、盈亏、盯盘、每日盈亏）由所有页面共用，15 秒刷新一次。
 *  页面切到后台时暂停轮询，回到前台立刻补一次；断网时保留旧数据，连续失败两次才提示。 */
export function OvProvider({ children }: { children: React.ReactNode }) {
  const [d, setD] = useState<Ov>({});
  const [loaded, setLoaded] = useState(false);
  const [offline, setOffline] = useState("");
  const [refreshing, setRefreshing] = useState(false);
  const [updated, setUpdated] = useState<number | null>(null);
  const fails = useRef(0);
  const inFlight = useRef(false);

  const refresh = useCallback(async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setRefreshing(true);
    try {
      const r = await api<Ov>("/api/overview");
      setD(r);
      setLoaded(true);
      setOffline("");
      setUpdated(Date.now());
      fails.current = 0;
    } catch {
      fails.current += 1;
      if (fails.current >= 2) setOffline(navigator.onLine ? "后台暂时没响应，重试中" : "手机/电脑没有联网");
    } finally {
      inFlight.current = false;
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    refresh();
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") refresh();
    }, POLL_MS);
    const onVisible = () => {
      if (document.visibilityState === "visible") refresh();
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("online", refresh);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("online", refresh);
    };
  }, [refresh]);

  return <C.Provider value={{ d, loaded, offline, refreshing, updated, refresh }}>{children}</C.Provider>;
}
