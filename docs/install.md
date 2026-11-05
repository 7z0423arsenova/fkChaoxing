# 源码运行前：安装 Rust 和 Go

只运行命令行版需要 **Rust 1.85 或更新版本**；从源码运行桌面版还需要 **Go 1.27.3 或更新版本**。这两个版本分别来自项目的 `Cargo.toml` 和 `desktop/go.mod`。已构建的桌面安装包不需要先安装语言工具链。

## 安装 Rust

Linux 或 macOS 打开终端，运行 [Rust 官方安装命令](https://rust-lang.org/tools/install)：

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

按安装程序提示完成后，重新打开终端。Windows 请到 [Rust 安装页](https://rust-lang.org/tools/install) 下载适合电脑架构的 `rustup-init.exe` 并运行；如果提示安装 Visual Studio C++ Build Tools，请按提示安装，然后重新打开 PowerShell。macOS 如果编译时报缺少链接器，可运行 `xcode-select --install` 安装命令行工具；Linux 遇到相同问题时按发行版说明安装 C 编译器。

检查版本：

```sh
rustc --version
cargo --version
```

## 安装 Go（桌面版需要）

从 [Go 官方下载页](https://go.dev/dl/) 选择适合系统和 CPU 架构的 **1.27.3 或更新版本**，按 [官方安装说明](https://go.dev/doc/install) 操作：

| 系统 | 安装方式 |
| --- | --- |
| Windows | 下载 `.msi` 并按向导安装，随后重新打开 PowerShell。 |
| macOS | 下载与 Intel 或 Apple 芯片对应的 `.pkg` 并按向导安装，随后重新打开终端。 |
| Linux | 下载与 CPU 架构对应的 `.tar.gz`，解压到 `/usr/local`，并把 `/usr/local/go/bin` 加入 `PATH`。如果 `/usr/local/go` 已有旧版本，先按官方说明替换，不能直接覆盖解压。 |

Linux x86-64 的新安装示例（假设下载文件位于 `~/Downloads`）：

```sh
sudo tar -C /usr/local -xzf ~/Downloads/go1.27.3.linux-amd64.tar.gz
export PATH="$PATH:/usr/local/go/bin"
go version
```

Linux ARM64 请改用下载页上的 ARM64 文件名。要让 `go` 在下次打开终端后仍可用，把上面的 `export PATH=...` 行加入 `~/.profile` 或正在使用的 shell 配置文件。Windows 和 macOS 的安装器会设置 Go 的命令路径。

安装后在新终端运行 `go version`，确认版本至少为 1.27.3。桌面项目已在 `desktop/go.mod` 指定 MyGo 依赖；执行教程中的 `go run .` 时，Go 会下载所需模块，无需另装 MyGo 命令。

## 安装 Python 与 ddddocr（可选）

需要自动识别学习通反爬图片验证码时，再安装 Python 3.9 或更新版本和 ddddocr：

```sh
python -m pip install ddddocr
```

ddddocr 完全在本机离线运行，不需要 API Key。安装后在 `config.ini` 中设置 `[captcha] enabled = true`，或在桌面版配置向导的“学习设置”里勾选“自动识别图片验证码”。如果 `python` 不在 `PATH`，用 `[captcha] python` 指定解释器路径。滑块、人脸等验证仍需在浏览器完成。

## 桌面窗口的系统组件

桌面版使用系统 WebView。根据 [MyGo 官方要求](https://mygo.egoist.dev/docs/getting-started)，Linux 需要 GTK 3 和 WebKitGTK 4.1；Debian/Ubuntu 可先安装 `libwebkit2gtk-4.1-0`，其他发行版安装对应包。Windows 10 需要 [WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/)；Windows 11 已包含。macOS 使用系统提供的 WebView。

装好后回到[桌面版教程](../desktop/README.md)或[命令行教程](cli.md)继续。
