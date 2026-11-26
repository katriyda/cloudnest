// CloudNest client — Alpine.js components + htmx bridge + upload pipeline.
(function () {
  'use strict';

  var $ = function (sel, root) { return (root || document).querySelector(sel); };
  var $$ = function (sel, root) { return Array.prototype.slice.call((root || document).querySelectorAll(sel)); };
  var csrf = function () {
    var meta = $('meta[name="csrf-token"]');
    return meta ? meta.content : '';
  };
  var fmtBytes = function (n) {
    if (!n && n !== 0) return '';
    if (n < 1024) return n + ' B';
    var u = ['KB', 'MB', 'GB', 'TB'];
    var v = n, i = -1;
    while (v >= 1024 && i < 3) { v /= 1024; i++; }
    return v.toFixed(v >= 10 ? 1 : 2) + ' ' + u[Math.max(0, i)];
  };

  document.addEventListener('alpine:init', function () {
    Alpine.data('app', function () {
      return {
        theme: document.documentElement.getAttribute('data-theme') === 'dark' ? 'dark' : 'light',
        drawer: false,
        confirm: { open: false, message: '', form: null },
        init: function () {
          var self = this;
          this.$watch('theme', function (value) {
            document.documentElement.setAttribute('data-theme', value);
            try { localStorage.setItem('cn-theme', value); } catch (e) {}
          });
          document.addEventListener('keydown', function (event) {
            if (event.key === 'Escape') {
              self.drawer = false;
              if (self.confirm.open) self.closeConfirm();
            }
          });
        },
        toggleTheme: function () {
          this.theme = this.theme === 'dark' ? 'light' : 'dark';
        },
        openDrawer: function () { this.drawer = true; },
        closeDrawer: function () { this.drawer = false; },
        askConfirm: function (form) {
          this.confirm = { open: true, message: form.getAttribute('data-confirm') || '', form: form };
        },
        closeConfirm: function () {
          this.confirm.open = false;
          this.confirm.form = null;
        },
        sidebarOpen: function () { return this.drawer; },
        submitConfirm: function () {
          var form = this.confirm.form;
          this.closeConfirm();
          if (form) form.submit();
        }
      };
    });
  });

  document.addEventListener('alpine:init', function () {
    Alpine.data('rowMenu', function () {
      return {
        open: false,
        toggle: function () { this.open = !this.open; },
        close: function () { this.open = false; }
      };
    });
  });

  // Destructive actions go through the in-page confirm dialog instead of window.confirm().
  document.addEventListener('submit', function (event) {
    var form = event.target;
    if (!(form instanceof HTMLFormElement)) return;
    var message = form.getAttribute('data-confirm');
    if (!message) return;
    event.preventDefault();
    event.stopImmediatePropagation();
    var root = document.body;
    if (window.Alpine && Alpine.$data) {
      Alpine.$data(root).askConfirm(form);
    } else if (window.confirm(message)) {
      form.submit();
    }
  }, true);

  // Copy-to-clipboard buttons.
  $$('[data-copy]').forEach(function (btn) {
    btn.addEventListener('click', async function () {
      var text = btn.getAttribute('data-copy') || '';
      try {
        await navigator.clipboard.writeText(text);
        btn.dataset.copied = '1';
        var old = btn.textContent;
        btn.textContent = '已复制';
        setTimeout(function () { btn.textContent = old; delete btn.dataset.copied; }, 1600);
      } catch (e) {
        window.prompt('复制这个链接：', text);
      }
    });
  });

  var uploadClose = document.querySelector('[data-upload-close]');
  if (uploadClose) {
    uploadClose.addEventListener('click', function () {
      var panel = uploadClose.closest('.upload-panel');
      if (panel) panel.hidden = true;
    });
  }

  // Flash messages auto-dismiss; Alpine only owns the shell state.
  document.querySelectorAll('[data-toast]').forEach(function (toast) {
    var close = toast.querySelector('[data-toast-close]');
    function hide() { toast.remove(); }
    if (close) close.addEventListener('click', hide);
    setTimeout(hide, 4200);
  });

  // Modal host: htmx swaps dialog markup into #modal, then we open the overlay.
  var modal = $('#modal');
  var returnFocus = null;
  function openModal() {
    if (!modal) return;
    if (!modal.classList.contains('is-open')) returnFocus = document.activeElement;
    modal.classList.add('is-open');
    document.body.style.overflow = 'hidden';
    var target = modal.querySelector('input:not([type="hidden"]), select, textarea');
    if (target) setTimeout(function () { target.focus(); }, 30);
    else setTimeout(function () {
      var close = modal.querySelector('[data-modal-close]');
      if (close) close.focus();
    }, 30);
  }
  function closeModal() {
    if (!modal) return;
    var wasOpen = modal.classList.contains('is-open');
    modal.classList.remove('is-open');
    modal.innerHTML = '';
    document.body.style.overflow = '';
    if (wasOpen && returnFocus && typeof returnFocus.focus === 'function') {
      var el = returnFocus;
      returnFocus = null;
      setTimeout(function () { el.focus(); }, 30);
    }
  }
  if (modal) {
    modal.addEventListener('click', function (event) {
      if (event.target === modal || event.target.closest('[data-modal-close]')) closeModal();
    });
    document.addEventListener('keydown', function (event) {
      if (event.key === 'Escape' && modal.classList.contains('is-open')) closeModal();
    });
    var lastModalHtml = '';
    document.body.addEventListener('htmx:after:swap', function () {
      var html = modal.innerHTML.trim();
      if (html === lastModalHtml) return;
      lastModalHtml = html;
      if (html !== '') {
        openModal();
        if (window.Alpine && Alpine.initTree) Alpine.initTree(modal);
      } else {
        closeModal();
      }
    });
    document.body.addEventListener('htmx:response:error', closeModal);
  }

  // Upload: multipart below the threshold, resumable 4 MiB chunks above it.
  var pageData = $('#page-data');
  if (pageData && pageData.dataset.page === 'files') {
    var parentId = pageData.dataset.parentId || '0';
    var uploadUrl = pageData.dataset.uploadUrl || '/files/upload';
    var fileInputs = $('[data-file-input]');
    var folderInputs = $('[data-folder-input]');
    var pane = $('#files-pane');
    var panel = $('#upload-panel');
    var counter = $('#upload-count');
    var fileNameEl = $('#upload-file-name');
    var statusEl = $('#upload-status');
    var percentEl = $('#upload-percent');
    var meterEl = $('#upload-meter');
    var uploading = false;
    var CHUNK_SIZE = 4 * 1024 * 1024;
    var CHUNKED_THRESHOLD = 16 * 1024 * 1024;

    function showPanel() { if (panel) panel.hidden = false; }
    function setProgress(name, done, total, text) {
      if (fileNameEl) fileNameEl.textContent = name;
      var pct = total > 0 ? Math.min(100, Math.round(done / total * 100)) : 0;
      if (meterEl) meterEl.style.width = pct + '%';
      if (percentEl) percentEl.textContent = pct + '%';
      if (statusEl) statusEl.textContent = text || (fmtBytes(done) + ' / ' + fmtBytes(total));
    }
    function fail(message) {
      if (statusEl) statusEl.textContent = message;
      if (meterEl) { meterEl.style.width = '100%'; meterEl.style.background = 'var(--danger)'; }
      uploading = false;
    }

    function uploadMultipart(file, relDir) {
      return new Promise(function (resolve, reject) {
        var xhr = new XMLHttpRequest();
        xhr.open('POST', uploadUrl);
        xhr.responseType = 'text';
        xhr.upload.onprogress = function (event) {
          if (event.lengthComputable) setProgress(file.name, event.loaded, event.total, '上传中…');
        };
        xhr.onload = function () {
          if (xhr.status >= 200 && xhr.status < 400) resolve();
          else reject(new Error('上传没有完成，请重试'));
        };
        xhr.onerror = function () { reject(new Error('网络连接中断，请重试')); };
        var form = new FormData();
        form.append('csrf', csrf());
        form.append('parent_id', parentId);
        form.append('path', relDir || '');
        form.append('files', file, file.name);
        xhr.send(form);
      });
    }

    async function initChunked(file, relDir) {
      var body = new URLSearchParams();
      body.set('csrf', csrf());
      body.set('parent_id', parentId);
      body.set('path', relDir || '');
      body.set('name', file.name);
      body.set('size', String(file.size));
      var response = await fetch('/api/upload/init', {
        method: 'POST',
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        body: body
      });
      if (!response.ok) throw new Error('上传没有开始，请检查网络后重试');
      return await response.json();
    }

    async function chunkStatus(token) {
      var response = await fetch('/api/upload/' + encodeURIComponent(token), { headers: { 'X-CSRF-Token': csrf() } });
      if (!response.ok) return null;
      return await response.json();
    }

    function putChunk(token, offset, blob) {
      return new Promise(function (resolve, reject) {
        var xhr = new XMLHttpRequest();
        xhr.open('POST', '/api/upload/chunk/' + encodeURIComponent(token));
        xhr.setRequestHeader('X-CSRF-Token', csrf());
        xhr.setRequestHeader('X-Offset', String(offset));
        xhr.setRequestHeader('Content-Type', 'application/octet-stream');
        xhr.onload = function () {
          if (xhr.status < 200 || xhr.status >= 300) { reject(new Error('上传中断，请重试')); return; }
          var data = null;
          try { data = JSON.parse(xhr.responseText); } catch (e) {}
          resolve(data);
        };
        xhr.onerror = function () { reject(new Error('网络连接中断，请重试')); };
        xhr.send(blob);
      });
    }

    async function uploadChunked(file, relDir) {
      var fingerprint = 'cn-up-' + [parentId, relDir || '', file.name, file.size, file.lastModified].join('|');
      var token = null, received = 0;
      try { token = localStorage.getItem(fingerprint); } catch (e) {}
      if (token) {
        var status = await chunkStatus(token).catch(function () { return null; });
        if (status && status.size === file.size) {
          received = status.received || 0;
          setProgress(file.name, received, file.size, '继续上传…');
        } else {
          token = null;
          try { localStorage.removeItem(fingerprint); } catch (e) {}
        }
      }
      if (!token) {
        var init = await initChunked(file, relDir);
        token = init.token;
        received = init.received || 0;
        try { localStorage.setItem(fingerprint, token); } catch (e) {}
      }
      while (received < file.size) {
        var end = Math.min(file.size, received + CHUNK_SIZE);
        var blob = file.slice(received, end);
        var attempts = 0;
        while (true) {
          try {
            var result = await putChunk(token, received, blob);
            received = result && typeof result.received === 'number' ? result.received : end;
            break;
          } catch (err) {
            attempts++;
            if (attempts >= 4) throw err;
            var s = await chunkStatus(token).catch(function () { return null; });
            if (s && s.size === file.size) {
              var serverReceived = Number(s.received) || 0;
              if (serverReceived > received) { received = serverReceived; break; }
            }
            await new Promise(function (resolve) { setTimeout(resolve, 700 * attempts); });
          }
        }
        setProgress(file.name, received, file.size, '上传中…');
      }
      var done = await fetch('/api/upload/complete/' + encodeURIComponent(token), {
        method: 'POST',
        headers: { 'X-CSRF-Token': csrf() }
      });
      if (!done.ok) throw new Error('上传完成确认失败，请刷新页面检查文件是否已上传');
      try { localStorage.removeItem(fingerprint); } catch (e) {}
    }

    async function uploadBatch(items) {
      if (uploading || !items.length) return;
      uploading = true;
      showPanel();
      if (meterEl) meterEl.style.background = '';
      if (counter) counter.textContent = '(' + items.length + ' 项)';
      var index = 0;
      for (var i = 0; i < items.length; i++) {
        var item = items[i];
        index++;
        try {
          if (item.file.size > CHUNKED_THRESHOLD) await uploadChunked(item.file, item.relDir);
          else await uploadMultipart(item.file, item.relDir);
        } catch (err) {
          fail((err && err.message) || '上传没有完成，请重试');
          uploading = false;
          return;
        }
        if (counter) counter.textContent = '(' + index + ' / ' + items.length + ')';
      }
      uploading = false;
      setProgress('上传完成', 1, 1, '全部文件已上传');
      setTimeout(function () { window.location.reload(); }, 450);
    }

    function filesFromList(list, relative) {
      return Array.prototype.map.call(list, function (file) {
        var relDir = '';
        var relPath = file.webkitRelativePath || '';
        if (relative && relPath) {
          var parts = relPath.split('/');
          parts.pop();
          relDir = parts.join('/');
        }
        return { file: file, relDir: relDir };
      });
    }

    fileInputs.forEach(function (input) {
      input.addEventListener('change', function () {
        uploadBatch(filesFromList(input.files, false));
        input.value = '';
      });
    });
    folderInputs.forEach(function (input) {
      input.addEventListener('change', function () {
        uploadBatch(filesFromList(input.files, true));
        input.value = '';
      });
    });

    if (pane) {
      var dragDepth = 0;
      pane.addEventListener('dragenter', function (event) {
        if (!event.dataTransfer || Array.prototype.indexOf.call(event.dataTransfer.types || [], 'Files') < 0) return;
        event.preventDefault();
        dragDepth++;
        document.body.classList.add('dragging');
      });
      pane.addEventListener('dragover', function (event) {
        event.preventDefault();
        if (event.dataTransfer) event.dataTransfer.dropEffect = 'copy';
      });
      pane.addEventListener('dragleave', function () {
        dragDepth = Math.max(0, dragDepth - 1);
        if (dragDepth === 0) document.body.classList.remove('dragging');
      });
      pane.addEventListener('drop', function (event) {
        event.preventDefault();
        dragDepth = 0;
        document.body.classList.remove('dragging');
        if (!event.dataTransfer) return;
        uploadBatch(filesFromList(event.dataTransfer.files, false));
      });
    }
  }

  // Site settings: online update flow.
  if (pageData && pageData.dataset.page === 'settings') {
    var updatePanel = $('#update-panel');
    var updateState = $('#update-state');
    var checkBtn = $('#update-check');
    var applyBtn = $('#update-apply');
    var supported = !!(updatePanel && updatePanel.dataset.supported === 'true');
    var latestVersion = null;
    var working = false;

    var setUpdateState = function (text, kind) {
      if (!updateState) return;
      updateState.textContent = text;
      updateState.classList.toggle('is-error', kind === 'error');
      updateState.classList.toggle('is-ok', kind === 'ok');
    };
    var readJson = async function (response) {
      var data = null;
      try { data = await response.json(); } catch (e) {}
      if (!data) throw new Error(response.ok ? '服务器返回了无法识别的内容' : '请求没有完成，请刷新页面后重试');
      return data;
    };
    var sleep = function (ms) { return new Promise(function (resolve) { setTimeout(resolve, ms); }); };

    async function checkForUpdate() {
      if (working || !checkBtn) return;
      working = true;
      checkBtn.disabled = true;
      if (applyBtn) applyBtn.hidden = true;
      setUpdateState('正在检查最新版本…');
      try {
        var response = await fetch('/admin/update/check', { headers: { Accept: 'application/json' }, cache: 'no-store' });
        var data = await readJson(response);
        latestVersion = data.latest;
        if (data.has_update) {
          setUpdateState('发现新版本 ' + data.latest + '（当前 ' + data.current + '）。');
          if (supported && applyBtn) applyBtn.hidden = false;
        } else {
          setUpdateState('已经是最新版本（' + data.current + '）。', 'ok');
        }
      } catch (err) {
        setUpdateState((err && err.message) || '检查更新失败，请稍后重试', 'error');
      } finally {
        working = false;
        checkBtn.disabled = false;
      }
    }

    async function applyUpdate() {
      if (working || !latestVersion || !applyBtn) return;
      if (!window.confirm('确定安装 ' + latestVersion + ' 吗？更新会替换程序并重启服务，期间连接会短暂中断。')) return;
      working = true;
      if (checkBtn) checkBtn.disabled = true;
      applyBtn.disabled = true;
      setUpdateState('正在下载并校验 ' + latestVersion + '…这可能需要几分钟。');
      try {
        var body = new URLSearchParams();
        body.set('csrf', csrf());
        var response = await fetch('/admin/update/apply', {
          method: 'POST',
          headers: { 'Content-Type': 'application/x-www-form-urlencoded', Accept: 'application/json' },
          body: body
        });
        var data = await readJson(response);
        if (!data.ok) {
          setUpdateState(data.message || '更新没有完成', 'error');
          working = false;
          if (checkBtn) checkBtn.disabled = false;
          applyBtn.disabled = false;
          return;
        }
        setUpdateState(data.message || '更新已安装，等待服务重启…', 'ok');
        await waitForVersion(data.version);
      } catch (err) {
        setUpdateState((err && err.message) || '更新没有完成，请查看服务日志', 'error');
        working = false;
        if (checkBtn) checkBtn.disabled = false;
        applyBtn.disabled = false;
      }
    }

    async function waitForVersion(expected) {
      for (var attempt = 0; attempt < 90; attempt++) {
        await sleep(1200);
        try {
          var response = await fetch('/api/version', { cache: 'no-store', headers: { Accept: 'application/json' } });
          if (response.ok) {
            var data = await response.json();
            if (data.version === expected) {
              setUpdateState('已更新到 ' + expected + '，页面即将刷新。', 'ok');
              setTimeout(function () { window.location.reload(); }, 900);
              return;
            }
            setUpdateState('服务已恢复，正在等待新版本生效…');
          }
        } catch (e) {
          setUpdateState('服务重启中…');
        }
      }
      setUpdateState('尚未确认新版本启动，请刷新页面或查看服务日志。', 'error');
      working = false;
      if (checkBtn) checkBtn.disabled = false;
      applyBtn.disabled = false;
    }

    if (checkBtn) checkBtn.addEventListener('click', checkForUpdate);
    if (applyBtn) applyBtn.addEventListener('click', applyUpdate);
  }
})();