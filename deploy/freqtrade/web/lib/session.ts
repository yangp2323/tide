/** 可在 Edge middleware 和 Node Route Handlers 中使用的签名会话。 */
export const COOKIE = "__Host-tide_session";
export const TTL = 12 * 60 * 60;
const encoder = new TextEncoder();
async function key() {
  const secret = process.env.PANEL_SESSION_SECRET;
  if (!secret) throw new Error("Missing PANEL_SESSION_SECRET");
  return crypto.subtle.importKey("raw", encoder.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
}
function hex(bytes: ArrayBuffer) {
  return Array.from(new Uint8Array(bytes), b => b.toString(16).padStart(2, "0")).join("");
}
export async function createSession() {
  const nonce = hex(crypto.getRandomValues(new Uint8Array(24)).buffer);
  const data = `${Math.floor(Date.now() / 1000) + TTL}.${nonce}`;
  return `${data}.${hex(await crypto.subtle.sign("HMAC", await key(), encoder.encode(data)))}`;
}
export async function validSession(token?: string) {
  if (!token) return false;
  const [expiry, nonce, sig, extra] = token.split(".");
  if (extra || !/^\d+$/.test(expiry) || !/^[a-f0-9]{48}$/.test(nonce || "") || !/^[a-f0-9]{64}$/.test(sig || "")) return false;
  if (Number(expiry) <= Date.now() / 1000) return false;
  const bytes = Uint8Array.from(sig.match(/../g)!, v => parseInt(v, 16));
  return crypto.subtle.verify("HMAC", await key(), bytes, encoder.encode(`${expiry}.${nonce}`));
}
