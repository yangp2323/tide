import { NextResponse } from "next/server";
import { COOKIE } from "@/lib/session";
export async function POST() {
  const response = NextResponse.json({ ok: true });
  response.cookies.set(COOKIE, "", { httpOnly: true, secure: true, sameSite: "strict", maxAge: 0, path: "/" });
  return response;
}
