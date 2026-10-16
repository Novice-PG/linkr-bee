# macOS 产物

这里放 macOS 的两份产物：

- `linkr-1.0.0-x86_64-apple-darwin.tar.gz`（Intel）
- `linkr-1.0.0-aarch64-apple-darwin.tar.gz`（Apple Silicon）

当前状态：**待 CI 构建**。打包机是 Linux/aarch64，无法交叉链接 macOS
二进制（需要 Apple 的 SDK），所以这两份由仓库的 GitHub Actions 工作流
`.github/workflows/linkr-tui.yml` 在 macOS 运行器上构建：推一个
`linkr-tui-v1.0.0` tag（或在 Actions 页手动触发），发布附件里就会带上它们。

## 自己构建

任意一台 Mac（先 `xcode-select --install` 装好命令行工具）：

```sh
cd linkr-bee/tools/linkr-tui
cargo build --release --locked
```

产物在 `target/release/linkr`，直接运行即可。首次运行若被 Gatekeeper
拦下，在「访达」里右击 → 打开，允许一次就行。
