/** 后台接口的薄封装：统一错误文案、登录过期处理。 */

export class ApiError extends Error {
  status: number;

  constructor(message: string, status = 0) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

type Opts = { method?: string; body?: unknown; signal?: AbortSignal };

function failMessage(status: number, data: any) {
  const d = data?.detail;
  if (typeof d === "string") return d;
  if (Array.isArray(d)) return d.map((x: any) => x?.msg).filter(Boolean).join("；") || `请求失败（${status}）`;
  return `请求失败（${status}）`;
}

export async function api<T = any>(path: string, opts: Opts = {}): Promise<T> {
  const { method = "GET", body, signal } = opts;
  let r: Response;
  try {
    r = await fetch(path, {
      method,
      signal,
      cache: "no-store",
      headers: body === undefined ? undefined : { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch (e) {
    if ((e as Error)?.name === "AbortError") throw e;
    throw new ApiError(navigator.onLine ? "连不上后台，请稍后再试" : "手机/电脑没有联网");
  }

  // 登录过期：Caddy 会重新弹登录框
  if (r.status === 401) {
    location.reload();
    throw new ApiError("登录已过期", 401);
  }

  const text = await r.text();
  let data: any = {};
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      // 反向代理或后台重启时返回的是 HTML
      if (!r.ok) throw new ApiError(`后台返回 ${r.status}，可能正在重启`, r.status);
    }
  }
  if (!r.ok) throw new ApiError(failMessage(r.status, data), r.status);
  return data as T;
}

export const post = <T = any>(path: string, body?: unknown) => api<T>(path, { method: "POST", body });
export const del = <T = any>(path: string) => api<T>(path, { method: "DELETE" });
