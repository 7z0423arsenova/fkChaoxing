# Resource 文件夹

此文件夹包含项目所需的资源文件：

- `font_map_table.json`：字体映射表，包含字体字符与其对应哈希值的映射关系，用于解密学习通的加密字体。数据来自 [Samueli924/chaoxing](https://github.com/Samueli924/chaoxing) 的 `resource/font_map_table.json`（GPL-3.0）。
- `ddddocr_worker.py`：可选的 ddddocr 常驻识别进程，用于自动识别学习通反爬图片验证码；编译时嵌入 Rust 二进制，需要本机安装 Python 与 ddddocr。

构建脚本 `build.rs` 会在编译时把它压缩为 gzip，程序运行时解压后建立查找表。源文件保持可读，方便审查和重新生成；二进制里只包含压缩数据。
