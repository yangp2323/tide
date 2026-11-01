/** 生产构建 → 静态导出 out/，由后台 FastAPI 直接托管，页面里的 /api 请求发给同一个域名。
 *  本地开发（next dev）时把 /api 反向代理到本机后台，见 PANEL_ORIGIN。 */
const dev = process.env.NODE_ENV === "development";
const origin = process.env.PANEL_ORIGIN || "http://127.0.0.1:8000";

/** @type {import('next').NextConfig} */
const config = {
  output: dev ? undefined : "export",
  trailingSlash: true,
  images: { unoptimized: true },
  reactStrictMode: true,
  poweredByHeader: false,
  async rewrites() {
    return dev ? [{ source: "/api/:path*", destination: `${origin}/api/:path*` }] : [];
  },
};

export default config;
