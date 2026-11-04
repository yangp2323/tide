# Tide 前端

Next.js App Router + React + TypeScript。所有页面在构建时静态导出，由 Rust 后端提供页面文件和 `/api/*` 接口；没有 Next.js 服务端进程、API Route Handler 或 middleware。

```bash
pnpm install --frozen-lockfile
pnpm typecheck
pnpm build
```

构建产物在 `out/`。生产部署时复制为发布目录的 `web/`，由 `TIDE_WEB_DIR` 指定。运行时不需要 Node.js 或 pnpm。

登录、会话、权限和 Origin 校验统一由 Rust 后端负责。API 保持原有页面所需的数据结构。不能仅用普通静态文件服务器替代 Rust 后端，否则 `/api` 和登录保护不会生效。

前端开发可运行 `pnpm dev`；完整功能调试使用静态构建搭配 Rust 后端与本地 HTTPS 反向代理，并设置对应的 `TIDE_PUBLIC_ORIGIN`。
