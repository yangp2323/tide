"""只接受 Next.js 服务转发的内部请求。公网认证由 Next.js 负责。"""
import hmac
import os
from fastapi import Request
from fastapi.responses import JSONResponse


def install(app):
    token = os.environ['PANEL_INTERNAL_TOKEN']
    if not token:
        raise RuntimeError('PANEL_INTERNAL_TOKEN is required')

    @app.middleware('http')
    async def internal_only(request: Request, call_next):
        supplied = request.headers.get('x-panel-internal-token', '')
        if not hmac.compare_digest(supplied.encode(), token.encode()):
            return JSONResponse({'detail': 'Forbidden'}, status_code=403)
        response = await call_next(request)
        response.headers['Cache-Control'] = 'no-store'
        return response
