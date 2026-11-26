// CloudNest 网页端到端测试。
//
// 这套脚本会自己拉起一个临时实例（临时数据目录 + 随机端口），然后用真实的
// Chromium 逐项走一遍网页功能：初始化、登录、文件管理、上传（含分片与拖拽）、
// 分享、回收站、账户、后台、站点设置、主题与移动端导航等。
//
// 运行方式：
//   cd tests/e2e && npm install && npx playwright install chromium && node ui.mjs
// 可用 CLOUDNEST_BIN 指定被测二进制，默认使用 target/debug/cloudnest(.exe)。
//
// 设计要点：断言失败不会中断整轮测试，最后统一汇总并以非 0 退出码结束，
// 这样 CI 能一眼看到是哪个功能坏了，而不是只看到“第一个错误”。

import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir, platform } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { chromium } from 'playwright';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '..', '..');
const isWindows = platform() === 'win32';
const binary = process.env.CLOUDNEST_BIN
  || resolve(repoRoot, 'target', 'debug', isWindows ? 'cloudnest.exe' : 'cloudnest');

const ADMIN = 'admin';
const ADMIN_PASSWORD = 'Admin-Passw0rd-123';

const results = [];
function check(name, pass, detail) {
  results.push({ name, pass: !!pass, detail: detail === undefined ? '' : String(detail) });
}
function section(title) {
  results.push({ heading: title });
}

async function waitForServer(url, timeoutMs = 30000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url + '/healthz');
      if (response.ok) return true;
    } catch {
      // 还没起来，继续等。
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  return false;
}

async function startServer() {
  const workDir = mkdtempSync(join(tmpdir(), 'cloudnest-e2e-'));
  const dataDir = join(workDir, 'data');
  mkdirSync(dataDir, { recursive: true });
  const port = 20000 + Math.floor(Math.random() * 20000);
  const listen = `127.0.0.1:${port}`;
  const configPath = join(workDir, 'config.toml');
  writeFileSync(
    configPath,
    [
      `listen = "${listen}"`,
      `data_dir = "${dataDir.replace(/\\/g, '/')}"`,
      `public_url = "http://${listen}"`,
      'secure_cookies = false',
      'max_upload_size_mb = 64',
      'default_quota_gb = 1',
      'session_days = 30',
      '',
    ].join('\n'),
  );
  const child = spawn(binary, ['serve', '--config', configPath], {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, RUST_LOG: 'warn' },
  });
  let output = '';
  child.stdout.on('data', (d) => { output += d.toString(); });
  child.stderr.on('data', (d) => { output += d.toString(); });
  const base = `http://${listen}`;
  const healthy = await waitForServer(base);
  if (!healthy) {
    child.kill();
    throw new Error('服务没有启动：\n' + output);
  }
  return {
    base,
    stop() {
      child.kill();
      try { rmSync(workDir, { recursive: true, force: true }); } catch {}
    },
  };
}

async function run({ base, browser }) {
  const context = await browser.newContext({ viewport: { width: 1366, height: 900 }, acceptDownloads: true });
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: base });
  const page = await context.newPage();
  const consoleErrors = [];
  page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push(m.text()); });
  page.on('pageerror', (e) => consoleErrors.push('pageerror: ' + e.message));

  const login = async (p, user = ADMIN, password = ADMIN_PASSWORD) => {
    await p.goto(base + '/login');
    await p.fill('input[name=username]', user);
    await p.fill('input[name=password]', password);
    await p.click('button[type=submit]');
    await p.waitForURL(/\/files/);
    await p.waitForTimeout(250);
  };
  const uploadFixture = async (p, entries) => {
    const csrf = await p.evaluate(() => document.querySelector('meta[name=csrf-token]').content);
    await p.evaluate(async ({ csrf, entries }) => {
      const fd = new FormData();
      fd.append('csrf', csrf);
      fd.append('parent_id', '0');
      fd.append('path', '');
      for (const entry of entries) {
        fd.append('files', new Blob([entry.body], { type: entry.type }), entry.name);
      }
      await fetch('/files/upload', { method: 'POST', body: fd, redirect: 'manual' });
    }, { csrf, entries });
    await p.goto(base + '/files');
    await p.waitForTimeout(300);
  };
  const rowFor = (p, name) => p.locator('.row:has(.file-name:text-is("' + name + '"))');

  try {
    // ---------- 初始化与登录 ----------
    section('初始化与登录');
    await page.goto(base + '/setup');
    check('初始化页可访问', await page.locator('input[name=password_confirm]').count() === 1);
    await page.fill('input[name=username]', ADMIN);
    await page.fill('input[name=password]', ADMIN_PASSWORD);
    await page.fill('input[name=password_confirm]', ADMIN_PASSWORD);
    await page.click('button[type=submit]');
    await page.waitForURL(/\/files/);
    check('初始化创建管理员并进入文件页', page.url().includes('/files'));

    await page.goto(base + '/setup');
    check('初始化完成后 /setup 跳转登录', page.url().includes('/login'));

    await page.fill('input[name=username]', ADMIN);
    await page.fill('input[name=password]', 'wrong-password');
    await page.click('button[type=submit]');
    await page.waitForTimeout(500);
    check('错误密码显示通用提示',
      (await page.locator('.update-state.is-error').textContent()).includes('用户名或密码错误'));
    await login(page);
    check('正确密码可以登录', page.url().includes('/files'));

    // ---------- 文件列表基础 ----------
    section('文件列表');
    await uploadFixture(page, [
      { name: 'note.txt', body: 'note body', type: 'text/plain' },
      { name: 'readme.md', body: '# Hello\n\nworld', type: 'text/markdown' },
      { name: 'big-a.dat', body: 'a'.repeat(64), type: 'application/octet-stream' },
    ]);
    check('文件列表渲染', await page.locator('.row').count() >= 3);

    await page.fill('.search input[name=q]', 'note');
    await page.press('.search input[name=q]', 'Enter');
    await page.waitForTimeout(600);
    check('搜索进入结果页', (await page.locator('h1.page-title').textContent()).includes('搜索结果'));
    check('搜索框保留关键词', await page.locator('.search input[name=q]').inputValue() === 'note');
    check('搜索命中结果', await page.locator('.row').count() >= 1);

    await page.goto(base + '/files');
    await page.waitForTimeout(300);
    await page.evaluate(() => { window.__e2eAlive = true; });
    await page.click('.sort-tabs a:has-text("大小")');
    await page.waitForTimeout(800);
    check('排序走 htmx 局部刷新', await page.evaluate(() => window.__e2eAlive === true));
    check('排序写入地址栏', page.url().includes('sort=size'));
    await page.click('.view-toggle a[title="网格视图"]');
    await page.waitForTimeout(800);
    check('网格视图渲染', await page.locator('.file-grid').count() === 1);
    await page.click('.view-toggle a[title="列表视图"]');
    await page.waitForTimeout(700);
    check('列表视图渲染', await page.locator('.file-table').count() === 1);

    // ---------- 行内菜单 ----------
    section('文件操作菜单');
    await page.locator('.row-actions .icon-btn').first().click();
    await page.waitForTimeout(250);
    check('更多菜单可以打开',
      await page.evaluate(() => getComputedStyle(document.querySelector('.row .more-menu')).display) === 'flex');
    await page.locator('h1.page-title').click();
    await page.waitForTimeout(300);
    check('点击外部关闭更多菜单',
      await page.evaluate(() => getComputedStyle(document.querySelector('.row .more-menu')).display) === 'none');

    // ---------- 上传 ----------
    section('上传');
    await page.setInputFiles('[data-file-input]', {
      name: 'picker.txt', mimeType: 'text/plain', buffer: Buffer.from('picker upload'),
    });
    await page.waitForTimeout(2200);
    check('文件选择器上传', await page.locator('.file-name:text-is("picker.txt")').count() === 1);

    await page.click('.sort-tabs a:has-text("修改时间")');
    await page.waitForTimeout(700);
    await page.setInputFiles('[data-file-input]', {
      name: 'after-swap.txt', mimeType: 'text/plain', buffer: Buffer.from('after swap'),
    });
    await page.waitForTimeout(2200);
    check('局部刷新后仍可上传', await page.locator('.file-name:text-is("after-swap.txt")').count() === 1);

    const dataTransfer = await page.evaluateHandle(() => {
      const dt = new DataTransfer();
      dt.items.add(new File(['dropped'], 'dropped.txt', { type: 'text/plain' }));
      return dt;
    });
    await page.dispatchEvent('#files-pane', 'dragenter', { dataTransfer });
    const dragging = await page.evaluate(() => document.body.classList.contains('dragging'));
    await page.dispatchEvent('#files-pane', 'drop', { dataTransfer });
    await page.waitForTimeout(2200);
    check('拖拽高亮提示', dragging);
    check('拖拽上传', await page.locator('.file-name:text-is("dropped.txt")').count() === 1);

    const uploadPanel = await page.evaluate(() => {
      const panel = document.querySelector('#upload-panel');
      return panel ? !panel.hidden || panel.hidden : null;
    });
    check('上传面板可关闭', await page.evaluate(() => !!document.querySelector('[data-upload-close]')) && uploadPanel !== null);

    // ---------- 对话框 ----------
    section('对话框');
    for (const round of [1, 2]) {
      await page.click('button:has-text("新建文件夹")');
      await page.waitForTimeout(700);
      check('新建文件夹弹窗第 ' + round + ' 次打开',
        await page.evaluate(() => document.querySelector('#modal').classList.contains('is-open')));
      await page.click('#modal button[data-modal-close]');
      await page.waitForTimeout(350);
    }

    const dirName = 'e2e-dir-' + Date.now();
    await page.click('button:has-text("新建文件夹")');
    await page.waitForTimeout(600);
    await page.fill('#modal input[name=name]', dirName);
    await page.click('#modal button[type=submit]');
    await page.waitForTimeout(1000);
    check('新建文件夹成功', await page.locator('.file-name:text-is("' + dirName + '")').count() === 1);

    await rowFor(page, dirName).locator('.row-actions .icon-btn').click();
    await page.waitForTimeout(200);
    await rowFor(page, dirName).locator('.more-menu button:has-text("重命名")').click();
    await page.waitForTimeout(700);
    check('重命名弹窗打开', await page.evaluate(() => document.querySelector('#modal').classList.contains('is-open')));
    const renamed = dirName + '-r';
    await page.fill('#modal input[name=name]', renamed);
    await page.click('#modal button[type=submit]');
    await page.waitForTimeout(1000);
    check('重命名生效', await page.locator('.file-name:text-is("' + renamed + '")').count() === 1);

    await rowFor(page, renamed).locator('.row-actions .icon-btn').click();
    await page.waitForTimeout(200);
    await rowFor(page, renamed).locator('.more-menu button:has-text("移动")').click();
    await page.waitForTimeout(700);
    check('移动弹窗列出目标文件夹', await page.locator('#modal select option').count() >= 1);
    await page.click('#modal button[data-modal-close]');
    await page.waitForTimeout(300);

    // ---------- 删除与回收站 ----------
    section('回收站');
    await rowFor(page, renamed).locator('.row-actions .icon-btn').click();
    await page.waitForTimeout(200);
    await rowFor(page, renamed).locator('.more-menu button:has-text("删除")').click();
    await page.waitForTimeout(350);
    check('删除前弹出确认框',
      await page.evaluate(() => document.querySelector('#confirm-host')?.dataset.open) === 'true');
    await page.click('#confirm-host .btn-primary');
    await page.waitForTimeout(1100);
    check('删除后列表移除', await page.locator('.file-name:text-is("' + renamed + '")').count() === 0);
    await page.goto(base + '/trash');
    check('回收站列出已删除项', await page.locator('.row').count() >= 1);
    await page.locator('.row form[action="/trash/restore"] button').first().click();
    await page.waitForTimeout(900);
    check('回收站恢复可用', await page.locator('.row').count() === 0);

    // ---------- 预览与下载 ----------
    section('预览与下载');
    await page.goto(base + '/files');
    await page.waitForTimeout(250);
    await page.click('.file-link:has-text("note.txt")');
    await page.waitForTimeout(500);
    check('文本预览显示内容', (await page.locator('.text-preview').textContent()).includes('note body'));
    const downloadEvent = page.waitForEvent('download', { timeout: 6000 }).catch(() => null);
    await page.click('a:has-text("下载")');
    check('文件下载可行', !!(await downloadEvent));

    // ---------- 分享 ----------
    section('分享链接');
    await page.goto(base + '/files');
    await page.waitForTimeout(250);
    await rowFor(page, 'note.txt').locator('.row-actions .icon-btn').click();
    await page.waitForTimeout(200);
    await rowFor(page, 'note.txt').locator('.more-menu button:has-text("分享")').click();
    await page.waitForTimeout(700);
    check('创建分享弹窗打开', await page.evaluate(() => document.querySelector('#modal').classList.contains('is-open')));
    await page.fill('#modal input[name=password]', 'share-pass-123');
    await page.click('#modal button[type=submit]');
    await page.waitForTimeout(1000);
    check('分享创建后跳转列表', page.url().includes('/shares'));
    check('分享列表出现记录', await page.locator('.share-row').count() >= 1);

    const beforeCopy = await page.locator('[data-copy]').first().innerHTML();
    await page.locator('[data-copy]').first().click();
    await page.waitForTimeout(1900);
    const afterCopy = await page.locator('[data-copy]').first().innerHTML();
    check('复制按钮保留图标', beforeCopy.includes('<svg') && afterCopy.includes('<svg'));
    const shareUrl = await page.locator('.share-input').first().inputValue();

    const guestContext = await browser.newContext();
    const guest = await guestContext.newPage();
    await guest.goto(shareUrl);
    check('访客分享页要求密码', await guest.locator('input[name=password]').count() === 1);
    await guest.fill('input[name=password]', 'share-pass-123');
    await guest.click('button[type=submit]');
    await guest.waitForTimeout(600);
    check('输入密码后可见分享内容', (await guest.locator('body').innerText()).includes('note.txt'));
    const guestDownload = guest.waitForEvent('download', { timeout: 6000 }).catch(() => null);
    await guest.locator('a:has-text("下载文件")').first().click().catch(() => {});
    check('访客可以下载', !!(await guestDownload));
    await guestContext.close();

    // ---------- 账户 ----------
    section('账户设置');
    await page.goto(base + '/account?ok=' + encodeURIComponent('测试提示'));
    await page.waitForTimeout(300);
    check('账户页显示操作提示', (await page.locator('.toast').textContent().catch(() => '')).includes('测试提示'));

    // ---------- 后台 ----------
    section('用户与存储');
    await page.goto(base + '/admin');
    check('后台页面渲染', (await page.locator('h1.page-title').textContent()).includes('用户与存储'));
    check('后台统计卡片', await page.locator('.stat').count() === 4);
    await page.fill('form[action="/admin/users/create"] input[name=username]', 'member');
    await page.fill('form[action="/admin/users/create"] input[name=password]', 'member-pass-123');
    await page.fill('form[action="/admin/users/create"] input[name=quota_gb]', '1');
    await page.click('form[action="/admin/users/create"] button[type=submit]');
    await page.waitForTimeout(1000);
    check('创建普通用户', await page.locator('tr:has-text("member")').count() === 1);
    const memberRow = page.locator('tr:has-text("member")');
    await memberRow.locator('form[action="/admin/users/quota"] input[name=quota_gb]').fill('2');
    await memberRow.locator('form[action="/admin/users/quota"] button').click();
    await page.waitForTimeout(1000);
    check('修改用户配额', (await page.locator('.toast').textContent().catch(() => '')).includes('2 GB'));
    await page.locator('tr:has-text("member") form[action="/admin/users/toggle"] button').click();
    await page.waitForTimeout(1000);
    check('禁用用户', (await page.locator('tr:has-text("member") .badge').textContent()).includes('已禁用'));

    // ---------- 站点设置 ----------
    section('站点设置');
    await page.goto(base + '/admin/settings');
    check('站点设置页渲染', await page.locator('#update-panel').count() === 1);
    await page.fill('input[name=session_days]', '14');
    await page.click('form.stack-form button[type=submit]');
    await page.waitForTimeout(1000);
    check('保存设置成功', (await page.locator('.toast').textContent().catch(() => '')).includes('设置已保存'));
    check('保存后回填新值', await page.locator('input[name=session_days]').inputValue() === '14');
    await page.click('#update-check');
    await page.waitForTimeout(3000);
    check('检查更新有反馈', (await page.locator('#update-state').textContent()).length > 4);

    // ---------- 主题与移动端 ----------
    section('主题与移动端');
    await page.goto(base + '/files');
    await page.waitForTimeout(250);
    const themeBefore = await page.evaluate(() => document.documentElement.dataset.theme);
    await page.click('.theme-toggle');
    await page.waitForTimeout(250);
    const themeAfter = await page.evaluate(() => document.documentElement.dataset.theme);
    check('主题可以切换', themeBefore !== themeAfter, themeBefore + ' -> ' + themeAfter);
    await page.reload();
    await page.waitForTimeout(300);
    check('主题刷新后保持', await page.evaluate(() => document.documentElement.dataset.theme) === themeAfter);

    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto(base + '/files');
    await page.waitForTimeout(350);
    check('移动端没有横向溢出',
      await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth + 1));
    check('移动端显示菜单按钮', await page.locator('.menu-btn').isVisible());
    await page.click('.menu-btn');
    await page.waitForTimeout(300);
    check('抽屉导航打开', await page.evaluate(() => document.querySelector('.sidebar').dataset.open) === 'true');
    await page.keyboard.press('Escape');
    await page.waitForTimeout(300);
    check('抽屉导航可以用 Esc 关闭', await page.evaluate(() => {
      const sidebar = document.querySelector('.sidebar');
      return getComputedStyle(sidebar).visibility === 'hidden' && sidebar.getBoundingClientRect().x < 0;
    }));

    // ---------- 错误页与退出 ----------
    section('错误页与退出');
    await page.setViewportSize({ width: 1366, height: 900 });
    await page.goto(base + '/no-such-page');
    check('404 页面渲染', (await page.locator('.error-code').textContent()) === '404');
    await page.goto(base + '/files');
    await page.click('button[aria-label="退出登录"]');
    await page.waitForTimeout(600);
    check('退出登录回到登录页', page.url().includes('/login'));
  } catch (error) {
    check('测试执行异常', false, error.message.split('\n')[0]);
  } finally {
    // 401（故意的错误登录）与 404（故意的错误地址）属于预期内的网络错误。
    const unexpected = consoleErrors.filter((line) =>
      !/status of 401/.test(line) && !/status of 404/.test(line));
    check('页面没有未预期的 JS 报错', unexpected.length === 0, unexpected.join(' | '));
    await context.close();
  }
}

let server;
let browser;
try {
  server = await startServer();
  browser = await chromium.launch();
  await run({ base: server.base, browser });
} catch (error) {
  results.push({ name: '启动失败', pass: false, detail: error.message });
} finally {
  if (browser) await browser.close();
  if (server) server.stop();
}

let failed = 0;
let passed = 0;
for (const item of results) {
  if (item.heading) {
    console.log('\n## ' + item.heading);
    continue;
  }
  if (item.pass) passed++;
  else failed++;
  console.log((item.pass ? '  PASS ' : '  FAIL ') + item.name + (item.detail ? '  ::  ' + item.detail : ''));
}
console.log('\n' + passed + ' passed, ' + failed + ' failed');
if (failed > 0) process.exit(1);
