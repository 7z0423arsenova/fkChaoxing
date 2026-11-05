# 命令行使用教程

命令行版只依赖 Rust，不需要安装 Go。

## 1. 编译

先按[安装教程](install.md)安装 Rust 1.85 或更新版本，然后在项目根目录执行：

```sh
cargo build --release
```

编译后的程序是 `./target/release/fkChaoxing`；Windows PowerShell 使用 `.\target\release\fkChaoxing.exe`。

## 2. 准备配置

把 [template.ini](../template.ini) 复制为 `config.ini`，至少填写账号：

```ini
[common]
use_cookies = false
username = 13800000000
password = 你的密码

[tiku]
provider =
```

`provider` 留空表示不启用题库：视频、文档、阅读等任务照常执行，章节检测会被跳过。需要答题时再按模板里的注释配置题库或大模型。

公开题库（GO题）不需要 Token，可以直接这样配：

```ini
[tiku]
provider = TikuGo
submit = false
cover_rate = 0.8
; 可选授权码；公共接口留空即可
; go_authorization =
; 查询间隔（秒）、失败重试次数、退避基数
go_min_interval = 1.0
go_retry_times = 3
go_retry_backoff = 1.2
```

题库没命中时想继续问大模型，用 `provider = TikuGo,AI`。自建开源题库则设 `provider = TikuAdapter`，并填写服务地址 `url`。

`config.ini` 默认从当前目录读取，也可以用 `-c` 指定其他位置。`cookies.txt`、`cache.json` 和日志始终写在当前工作目录。

### 自动识别图片验证码（可选）

学习通偶尔会返回图片验证码（反爬）。默认程序会停下等待人工处理；安装 [ddddocr](https://pypi.org/project/ddddocr/) 后，可以让程序自动识别并重发被拦截的请求：

```sh
python -m pip install ddddocr
```

然后在 `config.ini` 中开启：

```ini
[captcha]
enabled = true
; 可选：指定 Python 解释器；留空自动尝试 python3、python
; python = python3
; 单次验证最多识别几次（默认 6）
; max_retries = 6
; 每次识别前的等待秒数（默认 5）
; delay = 5
```

ddddocr 完全在本机运行，不需要联网或 API Key。它只处理学习通的图片字符验证码；滑块、人脸等验证仍需在浏览器完成。如果日志提示无法启动 ddddocr，先确认 `python -m pip install ddddocr` 能成功导入，再用 `python` 指定正确的解释器。桌面版配置向导的“学习设置”里也有同名开关。

## 3. 第一次运行

先列出课程：

```sh
./target/release/fkChaoxing courses -c config.ini
```

每行依次是**课程 ID、班级 ID、课程名**。用第一列的课程 ID 开始学习：

```sh
./target/release/fkChaoxing study -c config.ini --course-ids 123456789
```

多个课程用英文逗号分隔：

```sh
./target/release/fkChaoxing study -c config.ini --course-ids 123456789,123456790
```

默认视频倍速为 1、同时处理 4 个章节；可以临时调整：

```sh
./target/release/fkChaoxing study -c config.ini --course-ids 123456789 -s 1.5 -j 2
```

默认会先尝试把媒体进度直接上报到结尾（`instant_complete = true`），服务器确认后立即完成；失败会自动回退到正常播放。要严格按播放进度上报可以关闭：

```sh
./target/release/fkChaoxing study -c config.ini --course-ids 123456789 --instant-complete false
```

只测试题库，不跑课程：

```sh
./target/release/fkChaoxing ask "中国的首都是哪里？" -c config.ini \
  -t single -o "A. 上海" -o "B. 北京"
```

`ask` 的题型可选 `single`、`multiple`、`judgement`、`completion`、`short`。

## 4. 后面怎么查命令

不用背参数，直接查帮助：

```sh
./target/release/fkChaoxing --help
./target/release/fkChaoxing study --help
./target/release/fkChaoxing courses --help
./target/release/fkChaoxing ask --help
./target/release/fkChaoxing replay --help
```

配置项的完整说明在 `template.ini` 的注释里；更多日志用 `-v`（DEBUG）或 `-vv`（TRACE）：

```sh
./target/release/fkChaoxing study -c config.ini --course-ids 123456789 -vv
```

退出码：`0` 成功，`1` 运行失败，`2` 命令行参数错误。

## 5. 抓包重放音频/视频进度

`study` 直接构造进度上报报文；`replay` 走另一条路：先用浏览器真实播放一次，把播放器发出的进度请求抓成 HAR，再由程序按抓包顺序重放。适合任务已经播放过但没有确认完成，或者想用一份抓包补完本地/云端 MP3 对应任务的情况。

### 5.1 抓包

1. Chrome 或 Edge 打开学习通任务页面，按 F12 → Network，过滤 `multimedia/log`。
2. 从头播放目标音频/视频。媒体文件在本地还是云端都可以，重放只依赖报文，不依赖文件位置。
3. 播放到结尾后，在 Network 面板右键 → “Save all as HAR with content”，保存为 `capture.har`。

HAR 里包含 Cookie 和课程参数；仓库已默认忽略 `*.har`，不要提交或发给他人。

### 5.2 先预览

```sh
./target/release/fkChaoxing replay capture.har --dry-run
```

程序会按 HAR 的 `startedDateTime` 排序，列出匹配到的 `/mooc-ans/multimedia/log/` 报文。只看音频：

```sh
./target/release/fkChaoxing replay capture.har --media audio --dry-run
```

### 5.3 真实重放

```sh
./target/release/fkChaoxing replay capture.har --media audio
```

- 默认使用 `cookies.txt`；没有有效 Cookie 时回退到 HAR 里的 `Cookie` 头。
- `--login` 用配置文件里的账号密码重新登录并刷新 `cookies.txt`（抓包 Cookie 过期时用）。
- `--use-capture-cookies` 强制使用 HAR 里的 Cookie。
- 默认每条间隔 2000 ms；`--delay-ms 5000` 放慢，`--delay-ms 0` 不限速。
- 收到 `isPassed=true` 后停止；`--limit` 只发前 N 条，`--object-id` / `--jobid` 只发指定任务。
- 默认把 `_t` 刷新为当前时间（`enc` 不包含 `_t`，签名不受影响）；要原样保留加 `--keep-captured-t`。
- 服务器最后仍未确认完成时命令返回失败；抓包不完整时可加 `--allow-incomplete` 只做尽力重放。

注意：

- `enc` 与 `playingTime`/`duration` 绑定，重放不能改这两个值，否则程序会提示签名不一致，服务器也会拒绝。
- 服务器可能检查真实时间间隔。间隔太短被拒时，调大 `--delay-ms`，或直接用 `study` 正常播放。
- 只重放自己的账号和课程，并遵守平台规则。

## 6. 安全与合规

- `config.ini`、`cookies.txt`、`cache.json` 和 `fkChaoxing.log` 可能包含账号、会话或课程信息，不要提交到 Git，也不要发给他人。程序会把 Cookie 和日志权限收紧到仅当前用户可读。
- 请只操作自己的账号，并遵守学校和学习通平台的规定。
- 遇到验证码/安全验证时程序默认会停止；开启 `[captcha] enabled = true` 并安装 ddddocr 后，会尝试自动识别图片验证码并重发原请求。滑块、人脸等验证仍需在浏览器完成。日志会记录触发验证的接口 URL，并把完整页面保存为 `verification-<pid>-<时间>.html`，方便确认是登录、视频状态还是章节检测被拦。验证页面可能包含会话信息，不要分享或提交。
- 视频状态/进度接口返回 403 时，程序会先刷新 `dtoken` 和媒体状态，必要时用配置中的账号密码自动重新登录，并切换 `rt` 后重试；仍然失败才把章节交给上层的 5 次重试。重登使用独立 Cookie 会话，密码错误或验证码失败不会清掉其他并发章节正在用的会话；失败后 10 秒内不重复尝试。
- 自动提交的答案正确率无法保证；重要测验请人工核对。
- 倍速和并发都有平台限制，程序不会突破这些限制。遇到进度不确认或验证码时，建议先用 `-s 1 -j 1` 重试。
