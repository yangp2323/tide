# Tide · Next.js 全栈管理后台

公网入口为 Next.js App Router：页面、账号登录、会话、退出和 `/api/*` Route Handlers。Python FastAPI 保留交易管理与 AI 实现，仅接受带内部令牌的请求，不对外发布端口。Freqtrade 是独立交易引擎。

## 当前部署

浏览器 → Nginx SNI（443）→ Caddy（HTTPS）→ Next.js（web:3000）→ Python（panel:8000）→ Freqtrade（8080）。

Next.js 使用 `next dev --hostname 0.0.0.0`。`/opt/tide/web` 挂载至容器 `/app`，保存页面、组件及 Route Handler 后自动更新。依赖和构建缓存保存在 Docker volumes。字体位于 `public/fonts/`。

```bash
cd /opt/tide
docker compose up -d --build --no-deps web
docker compose logs -f web
docker compose exec web pnpm typecheck
```

修改依赖后重启 web 以重新安装；修改环境变量或 Dockerfile 后重新创建 web。Python 源文件更新仍需重启 panel；交易策略变更按 Freqtrade 的方式重新加载。

## 登录

登录页 `/login`，沿用管理员账号。密码通过 scrypt 校验，配置文件 `user_data/panel-auth.json` 只读挂载到 Next.js。`PANEL_SESSION_SECRET` 签署有效期 12 小时的 HttpOnly / Secure / SameSite Cookie；`PANEL_INTERNAL_TOKEN` 保护 Python 服务。这两个值保存在服务器 `.env`，不会传给浏览器。所有写请求校验 Origin。

登录失败有速率限制；退出会清除浏览器会话。会话密钥变更会使现有登录失效。

## 说明

当前按要求以开发模式运行以支持热更新。首次访问新页面时需要按需编译，速度低于生产构建。`pnpm build` 可验证生产构建；当前 Docker Compose 不使用静态导出或旧版 `panel/web`。
