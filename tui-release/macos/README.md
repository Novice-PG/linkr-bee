# macOS 产物

这里放 macOS 的两份产物，由仓库的 GitHub Actions 工作流
`.github/workflows/linkr-tui.yml` 在 macOS 运行器上构建，与 Releases 页
`linkr-tui-v1.0.0` 的附件同源：

- `linkr-1.0.0-aarch64-apple-darwin.tar.gz`（Apple Silicon）
- `linkr-1.0.0-x86_64-apple-darwin.tar.gz`（Intel）

校验和见上一级的 `SHA256SUMS`。

## 使用

按芯片选一份（M 系列用 `aarch64`，Intel 用 `x86_64`）：

```sh
tar xzf linkr-1.0.0-aarch64-apple-darwin.tar.gz
cd linkr-1.0.0-aarch64-apple-darwin
./linkr --version                                  # 应输出 linkr 1.0.0
./linkr                                             # 启动界面
```

首次运行若被 Gatekeeper 拦下，在「访达」里右击 → 打开，允许一次就行；
也可以在终端里 `xattr -d com.apple.quarantine linkr` 去掉隔离标记。

## 自己构建

任意一台 Mac（先 `xcode-select --install` 装好命令行工具）：

```sh
cd linkr-bee/tools/linkr-tui
cargo build --release --locked
```

产物在 `target/release/linkr`，直接运行即可。
