# 网页端到端测试

用真实 Chromium 把 CloudNest 网页功能走一遍，覆盖初始化、登录、文件管理、
上传（选择器 / 分片 / 拖拽）、分享、回收站、账户、后台、站点设置、主题与移动端导航。

脚本会自己拉起一个临时实例（临时数据目录 + 随机端口），不会碰现有数据。

## 本地运行

```bash
cd tests/e2e
npm install
npx playwright install chromium
node ui.mjs
```

默认使用 `target/debug/cloudnest`（Windows 为 `cloudnest.exe`），先执行
`cargo build`。也可以用环境变量指定被测程序：

```bash
CLOUDNEST_BIN=/path/to/cloudnest node ui.mjs
```

## 约定

- 断言失败不会中断整轮测试，最后汇总并以非 0 退出码结束，CI 能直接看出坏的是哪个功能。
- 新增网页功能时，请在 `ui.mjs` 里补一条对应的断言，避免“界面改过但没人点过”。
- 401（故意输错密码）和 404（故意访问不存在的地址）属于预期内的网络错误，不会记为失败。
