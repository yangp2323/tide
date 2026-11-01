/** 对外统一 API；Python 交易服务仅在 Docker 内网可访问。 */
import { NextRequest, NextResponse } from "next/server";
export const runtime = "nodejs";
export const dynamic = "force-dynamic";
async function proxy(request: NextRequest) {
  const url = new URL(request.nextUrl.pathname + request.nextUrl.search, process.env.PANEL_ORIGIN || "http://panel:8000");
  const headers = new Headers({ "x-panel-internal-token": process.env.PANEL_INTERNAL_TOKEN || "" });
  if (request.headers.has("content-type")) headers.set("content-type", request.headers.get("content-type")!);
  try {
    const upstream = await fetch(url, {
      method: request.method, headers, cache: "no-store", redirect: "manual",
      body: ["GET", "HEAD"].includes(request.method) ? undefined : await request.arrayBuffer(),
      signal: AbortSignal.timeout(120000),
    });
    return new NextResponse(upstream.body, { status: upstream.status, headers: {
      "Content-Type": upstream.headers.get("content-type") || "application/json",
      "Cache-Control": "no-store",
    } });
  } catch {
    return NextResponse.json({ detail: "交易服务暂时不可用，请稍后重试" }, { status: 502 });
  }
}
export { proxy as GET, proxy as POST, proxy as PUT, proxy as DELETE, proxy as PATCH };
