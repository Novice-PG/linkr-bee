Linkr Bee v0.1 — Web terminal
Source: https://github.com/radxa/linkr-bee/tree/v0.1
Commit: ad254e8832303073f1b53f258c1d250aeb4fc11b

Extract this folder and serve it locally:
  cd linkr-bee-v0.1-web
  python3 -m http.server 8765 --bind 127.0.0.1
Then open http://127.0.0.1:8765/ in Chrome/Chromium.
For a remote deployment, use HTTPS for Web Bluetooth. Opening index.html as
file:// is not supported. No npm install or external download service is needed.
AI setup requires a compatible model endpoint and the user's own credentials.

解压后进入目录，运行上述 Python 静态服务器命令，用 Chrome/Chromium 访问。
远程部署请使用 HTTPS；不要直接以 file:// 打开页面。
使用说明：https://github.com/radxa/linkr-bee/blob/v0.1/docs/USAGE.zh-CN.md
