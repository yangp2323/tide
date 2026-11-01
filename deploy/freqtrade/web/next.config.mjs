/** Next.js 全栈运行，API Route Handlers 转发至内部交易服务。 */
export default {
  reactStrictMode: true,
  poweredByHeader: false,
  images: { unoptimized: true },
  // 服务器上跑的是 next dev（源码挂载、保存即生效），开发浮标会挡住手机端底部导航
  devIndicators: false,
};
