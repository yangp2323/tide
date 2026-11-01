import { NextRequest, NextResponse } from "next/server";
import { COOKIE, validSession } from "@/lib/session";
export async function middleware(request: NextRequest) {
  const path = request.nextUrl.pathname;
  if (!["GET", "HEAD", "OPTIONS"].includes(request.method)) {
    const origin = request.headers.get("origin");
    const expected = `${request.headers.get("x-forwarded-proto") || "https"}://${request.headers.get("host")}`;
    if (origin !== expected) return NextResponse.json({ detail: "请求来源无效，请刷新页面后重试" }, { status: 403 });
  }
  const publicPath = path === "/login" || path === "/login/" || path === "/api/auth/login";
  if (!publicPath && !await validSession(request.cookies.get(COOKIE)?.value)) {
    if (path.startsWith("/api/")) return NextResponse.json({ detail: "登录已过期，请重新登录" }, { status: 401, headers: { "Cache-Control": "no-store" } });
    // 使用代理保留的公网 Host，避免重定向到容器地址。
    const url = new URL("/login", `${request.headers.get("x-forwarded-proto") || "https"}://${request.headers.get("host")}`);
    url.searchParams.set("next", path + request.nextUrl.search);
    const redirect = NextResponse.redirect(url, 303);
    redirect.headers.set("Cache-Control", "no-store");
    return redirect;
  }
  const response = NextResponse.next();
  response.headers.set("Cache-Control", "no-store");
  response.headers.set("X-Frame-Options", "DENY");
  response.headers.set("X-Content-Type-Options", "nosniff");
  response.headers.set("Referrer-Policy", "same-origin");
  return response;
}
export const config = { matcher: ["/((?!_next/|fonts/|favicon.ico).*)"] };
