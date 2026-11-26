# 第三方软件许可声明

CloudNest 自身以 MIT 许可证发布（见 `Cargo.toml` 中的 `license = "MIT"`）。本项目在构建与运行过程中使用了下列第三方软件，其版权与许可归原作者所有。

## htmx

- 版本：4.0.2
- 位置：`static/htmx.min.js`
- 项目主页：<https://htmx.org/>
- 源码仓库：<https://github.com/bigskysoftware/htmx>
- 许可证：Zero-Clause BSD（0BSD）
- 版权：Copyright (c) 2020 Big Sky Software

htmx 4 以 0BSD 许可证授权，允许无限制使用、修改与再分发。完整许可文本随二进制一并保存在 `static/htmx.LICENSE.txt`：

```text
Zero-Clause BSD

Permission to use, copy, modify, and/or distribute this software for
any purpose with or without fee is hereby granted.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL
WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES
OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE
FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY
DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN
AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT
OF OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
```

## Alpine.js

- 版本：3.17.5（CSP 构建，不使用 eval）
- 位置：`static/alpine.min.js`
- 项目主页：<https://alpinejs.dev/>
- 源码仓库：<https://github.com/alpinejs/alpine>
- 许可证：MIT
- 版权：Copyright © 2019-2025 Caleb Porzio and contributors

完整许可文本随二进制保存在 `static/alpine.LICENSE.txt`：

```text
MIT License

Copyright © 2019-2025 Caleb Porzio and contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Rust 依赖

服务端由 Rust 生态依赖构建，具体版本锁定在 `Cargo.lock`。这些 crates 的许可证以各自发布信息为准（常见为 MIT 或 Apache-2.0），分发二进制时请同时遵守其许可要求。可通过以下命令查看依赖树与许可证信息（需安装相应工具）：

```bash
cargo metadata --format-version 1
cargo install cargo-license --locked
cargo license
```

## 字体与图标

界面主字体与等宽字体以 woff2 形式随二进制内置，均由 `src/assets.rs` 通过 `include_bytes!` 嵌入，并以带内容哈希的 `/static/v/<version>/fonts/` 地址提供：

| 字体 | 用途 | 许可证 | 版权 |
| --- | --- | --- | --- |
| Space Grotesk | 界面主字体（拉丁字符） | SIL Open Font License 1.1 | Copyright 2020 The Space Grotesk Project Authors |
| IBM Plex Mono | 元信息、数值、路径等等宽排版 | SIL Open Font License 1.1 | Copyright 2017 IBM Corp. |

字体文件位于 `static/fonts/`，许可全文分别见 `static/fonts/LICENSE-space-grotesk.txt` 与 `static/fonts/LICENSE-ibm-plex-mono.txt`。中文与日韩字符回退到系统已安装的对应 CJK 字体（不随二进制分发）。

界面图标为项目自带 SVG（`templates/icons/`），无第三方图标集依赖。