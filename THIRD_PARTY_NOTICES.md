# 第三方软件许可声明

CloudNest 自身以 MIT 许可证发布（见 `Cargo.toml` 中的 `license = "MIT"`）。本项目在构建与运行过程中使用了下列第三方软件，其版权与许可归原作者所有。

## htmx

- 版本：以 `static/htmx.min.js` 内的 `htmx.version` 为准
- 位置：`static/htmx.min.js`
- 项目主页：<https://htmx.org/>
- 源码仓库：<https://github.com/bigskysoftware/htmx>
- 许可证：MIT
- 版权：Copyright (c) 2020 Big Sky Software

htmx 以 MIT 许可证授权，完整许可文本如下：

```text
MIT License

Copyright (c) 2020 Big Sky Software

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

界面使用系统字体与项目内置 SVG 图标；如引入第三方图标集或字体，请在发布前补充对应许可声明。
