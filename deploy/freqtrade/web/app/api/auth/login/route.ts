import { NextRequest, NextResponse } from "next/server";
import { readFile } from "node:fs/promises";
import { scrypt, timingSafeEqual } from "node:crypto";
import { promisify } from "node:util";
import { COOKIE, TTL, createSession } from "@/lib/session";
export const runtime = "nodejs";
const derive = promisify(scrypt);
const globalState = globalThis as typeof globalThis & { loginFailures?: number[] };
const failures = globalState.loginFailures ??= [];
export async function POST(request: NextRequest) {
  while (failures.length && failures[0] < Date.now() - 60000) failures.shift();
  if (failures.length >= 20) return NextResponse.json({ detail: "尝试次数过多，请一分钟后重试" }, { status: 429, headers: { "Retry-After": "60" } });
  let body: any;
  try { body = await request.json(); } catch { return NextResponse.json({ detail: "请输入账号和密码" }, { status: 400 }); }
  const { username, password } = body || {};
  if (typeof username !== "string" || typeof password !== "string" || password.length > 1024 || username.length > 256) return NextResponse.json({ detail: "账号或密码错误" }, { status: 400 });
  const credentials = JSON.parse(await readFile(process.env.PANEL_AUTH_FILE || "/run/panel-auth.json", "utf8"));
  const digest = await derive(password, Buffer.from(credentials.salt, "hex"), 64) as Buffer;
  if (username !== credentials.username || !timingSafeEqual(digest, Buffer.from(credentials.password_hash, "hex"))) {
    failures.push(Date.now());
    return NextResponse.json({ detail: "账号或密码错误" }, { status: 401 });
  }
  const response = NextResponse.json({ ok: true });
  response.cookies.set(COOKIE, await createSession(), { httpOnly: true, secure: true, sameSite: "strict", maxAge: TTL, path: "/" });
  return response;
}
