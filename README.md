# :computer: fkChaoxing · 超星学习通任务工具

> Rust + MyGo 桌面版已接入课程登录、章节任务和题库。首次使用建议从[命令行教程](docs/cli.md)或[桌面版教程](desktop/README.md)开始。核心尚未用真实课程做端到端验证。

Linux、macOS 和 Windows 的桌面构建由 [CI](.github/workflows/main.yml) 分别检查。

## 功能

- 课程登录、章节任务：视频、音频、文档、阅读、直播和章节检测答题。
- 题库答题：支持多个题库按顺序回退，也可以接入兼容 OpenAI 接口的大模型或本机 Ollama。
- 可选 ddddocr：自动识别学习通反爬图片验证码，默认关闭。
- 两种使用方式：命令行核心和 MyGo 桌面窗口。

## 桌面版使用

桌面端仍然需要从源码开始编译。先按[语言安装教程](docs/install.md)准备 Rust、Go 和桌面组件，完整步骤见[桌面版使用教程](desktop/README.md)。从源码启动时，在项目根目录运行：

```sh
cargo build
cd desktop
go run .
```

首次打开会显示配置向导。填入学习通账号和密码并保存，然后点“读取课程”，取消不想处理的课程，最后点“开始学习”。窗口也能单独测试题库；已有 `config.ini` 可直接在窗口中选择。需要手动编辑时参考[精简配置模板](template.ini)。

Linux、macOS 和 Windows 分别需要对应平台的 Rust 核心与 MyGo 桌面程序。构建安装包、配置题库、查看课程 ID 和排错步骤均写在[桌面版教程](desktop/README.md)。

## 命令行

Rust 核心也能独立运行。编译、列出课程、运行指定课程和单题查询的命令见[命令行教程](docs/cli.md)。

如果已经用浏览器真实播放过、想把抓到的进度报文按顺序补发，可以使用 `replay` 子命令，步骤见同一篇教程的「抓包重放音频/视频进度」。

## 配置

配置文件使用 UTF-8 INI 格式，模板见 [`template.ini`](template.ini)。常用设置包括：

- `[common]`：账号、课程 ID、倍速、并发章节数、章节检测答题开关。
- `[tiku]`：题库类型、自动提交、覆盖率，以及 AI 接口或本机 Ollama。
- `[captcha]`：可选的 ddddocr 验证码识别。

需要自动识别学习通反爬图片验证码时，先安装 [ddddocr](https://pypi.org/project/ddddocr/)：

```sh
python -m pip install ddddocr
```

然后在 `config.ini` 中开启：

```ini
[captcha]
enabled = true
; 可选：指定 Python 解释器；留空自动尝试 python3、python
; python = python3
; max_retries = 6
; delay = 5
```

桌面版配置向导的“学习设置”里也有同名开关。ddddocr 只处理图片字符验证码，滑块、人脸等验证仍需在浏览器完成。

## 卸载

卸载前先退出程序，确认没有正在运行的学习任务。

### 桌面版

**Linux（用 `install.sh` 安装）**

在当初解压安装包的目录执行：

```sh
sh install.sh --uninstall
```

它会删除 `~/.local/fkchaoxing.app`，以及应用菜单、MIME 和命令链接，但会保留配置和登录数据。

如果安装包已经删除，手动删除：

```sh
rm -rf ~/.local/fkchaoxing.app
rm -f ~/.local/bin/fkchaoxing
rm -f "${XDG_DATA_HOME:-$HOME/.local/share}/applications/fkchaoxing.desktop"
rm -f "${XDG_DATA_HOME:-$HOME/.local/share}/mime/packages/fkchaoxing.xml"
```

**Linux（`.deb` 安装）**

```sh
sudo apt remove fkchaoxing
```

**Windows / macOS**

关闭程序后，删除对应的安装目录或应用包即可。

### 用户数据

卸载程序不会自动删除账号、Cookie、日志和缓存。确认不再需要后，按平台删除 MyGo 用户数据目录：

| 系统 | 目录 |
| --- | --- |
| Linux | `~/.config/fkChaoxing`（旧版为 `~/.config/Chaoxing`） |
| macOS | `~/Library/Application Support/fkChaoxing`（旧版为 `~/Library/Application Support/Chaoxing`） |
| Windows | `%APPDATA%\fkChaoxing`（旧版为 `%APPDATA%\Chaoxing`） |

例如 Linux：

```sh
rm -rf ~/.config/fkChaoxing ~/.config/Chaoxing
```

桌面窗口会显示实际配置路径；如果设置过 `XDG_CONFIG_HOME` 或使用旧版目录，以窗口显示的路径为准。目录里通常有 `config.ini`、`cookies.txt`、`cache.json`、`fkChaoxing.log*`、`selected-config.json`，删除后不可恢复。

### ddddocr（可选）

如果 ddddocr 装在独立虚拟环境里，直接删除虚拟环境目录即可。例如：

```sh
rm -rf ~/.local/share/fkChaoxing/ddddocr-venv
```

如果装在系统 Python 中：

```sh
python -m pip uninstall ddddocr
```

不需要为了卸载 ddddocr 一并删除 numpy、onnxruntime、opencv 等公共依赖，除非你确认其他程序也不再使用它们。

### 源码运行

如果只是从源码运行，删除项目目录即可。想保留源码但清理构建产物和运行数据，在项目根目录执行：

```sh
cargo clean
rm -rf desktop/build \
  desktop/resources/*/bin/fkChaoxing-core* \
  config.ini config.local.ini cookies.txt cache.json \
  selected-config.json fkChaoxing.log* verification-*.html
```

`cargo clean` 只删除 Rust 构建产物；`config.ini`、`cookies.txt` 等文件包含账号和会话信息，删除后不可恢复。

## 参考项目与致谢

本项目在整体架构、任务流程和部分实现设计上参考了 [Samueli924/chaoxing](https://github.com/Samueli924/chaoxing)（GPL-3.0）。`resource/font_map_table.json` 字体映射数据集也来自该项目，用于解析学习通的加密字体。

感谢原作者及所有贡献者。本项目是独立的 Rust + MyGo 实现，不是该项目的官方分支；两处实现存在差异时，以本仓库代码为准。第三方来源说明见 [`NOTICE`](NOTICE)。

## 版权与许可

- 代码遵循 [GPL-3.0 License](LICENSE) 协议。禁止**使用本代码或者修改/换皮进行商业盈利**；基于本代码的项目也必须同样遵守 [GPL-3.0 License](LICENSE) 协议。
- 本代码用于**学习架构和个人练习使用**。
- 他人或组织使用本代码进行任何**违法行为**都和作者本人无关。

许可证全文见 [LICENSE](LICENSE)。
