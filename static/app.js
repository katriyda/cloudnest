// CloudNest client interactions
(function () {
  'use strict';

  const $ = (sel, root) => (root || document).querySelector(sel);
  const $$ = (sel, root) => Array.from((root || document).querySelectorAll(sel));
  const csrf = () => ($('meta[name="csrf-token"]') || {}).content || '';
  const pageData = () => $('#page-data');
  const fmtBytes = (n) => {
    if (!n && n !== 0) return '';
    if (n < 1024) return n + ' B';
    const u = ['KB', 'MB', 'GB', 'TB'];
    let v = n, i = -1;
    while (v >= 1024 && i < 3) { v /= 1024; i++; }
    return v.toFixed(v >= 10 ? 1 : 2) + ' ' + u[Math.max(0, i)];
  };

  // Theme
  function setTheme(theme) {
    document.documentElement.setAttribute('data-theme', theme);
    try { localStorage.setItem('cn-theme', theme); } catch (e) {}
  }
  function toggleTheme() {
    setTheme(document.documentElement.getAttribute('data-theme') === 'dark' ? 'light' : 'dark');
  }
  $$('[data-theme-toggle]').forEach((btn) => btn.addEventListener('click', toggleTheme));

  // Sidebar
  const sidebar = $('#sidebar');
  function setSidebar(open) {
    if (sidebar) {
      sidebar.classList.toggle('is-open', open);
      $$('[data-sidebar-open]').forEach((btn) => btn.setAttribute('aria-expanded', open ? 'true' : 'false'));
    }
    document.body.classList.toggle('sidebar-open', open);
  }
  $$('[data-sidebar-open]').forEach((btn) => btn.addEventListener('click', () => setSidebar(true)));
  $$('[data-sidebar-close]').forEach((el) => el.addEventListener('click', () => setSidebar(false)));
  $$('.nav-link').forEach((link) => link.addEventListener('click', () => setSidebar(false)));
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') setSidebar(false);
  });

  // Flash dismiss
  $$('[data-dismiss]').forEach((btn) => btn.addEventListener('click', () => {
    const flash = btn.closest('.flash');
    if (flash) flash.remove();
    else {
      const panel = btn.closest('.upload-panel');
      if (panel) panel.hidden = true;
    }
  }));

  // Confirmation for destructive actions
  document.addEventListener('submit', (event) => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement)) return;
    const message = form.getAttribute('data-confirm');
    if (message && !window.confirm(message)) {
      event.preventDefault();
      event.stopImmediatePropagation();
    }
  }, true);

  // Modal handling
  const modal = $('#modal');
  let modalReturnFocus = null;
  function openModal() {
    if (!modal) return;
    if (!modal.classList.contains('is-open')) modalReturnFocus = document.activeElement;
    modal.classList.add('is-open');
    document.body.style.overflow = 'hidden';
    const target = modal.querySelector('input:not([type="hidden"]), select, textarea');
    if (target) setTimeout(() => target.focus(), 20);
    else setTimeout(() => { const close = modal.querySelector('[data-modal-close]'); if (close) close.focus(); }, 20);
  }
  function closeModal() {
    if (!modal) return;
    const wasOpen = modal.classList.contains('is-open');
    modal.classList.remove('is-open');
    modal.innerHTML = '';
    document.body.style.overflow = '';
    if (wasOpen && modalReturnFocus && typeof modalReturnFocus.focus === 'function') {
      const element = modalReturnFocus;
      modalReturnFocus = null;
      setTimeout(() => element.focus(), 20);
    }
  }
  if (modal) {
    modal.addEventListener('click', (event) => {
      if (event.target === modal) closeModal();
    });
    modal.addEventListener('click', (event) => {
      if (event.target.closest('[data-modal-close]')) closeModal();
    });
    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && modal.classList.contains('is-open')) closeModal();
    });
    document.body.addEventListener('htmx:after:swap', () => {
      if (modal && modal.innerHTML.trim() !== '') openModal();
    });
    document.body.addEventListener('htmx:response:error', () => {
      if (modal) closeModal();
    });
  }

  // Copy share URL
  $$('[data-copy]').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const text = btn.getAttribute('data-copy') || '';
      const label = btn.innerHTML;
      try {
        await navigator.clipboard.writeText(text);
        btn.textContent = '已复制';
      } catch (e) {
        const area = document.createElement('textarea');
        area.value = text;
        area.setAttribute('readonly', '');
        area.style.position = 'fixed';
        area.style.top = '-1000px';
        document.body.appendChild(area);
        area.select();
        try { document.execCommand('copy'); btn.textContent = '已复制'; } catch (err) {}
        document.body.removeChild(area);
      }
      setTimeout(() => { btn.innerHTML = label; }, 1600);
    });
  });

  // Set datetime-local minimums
  const nowLocal = () => {
    const d = new Date();
    d.setMinutes(d.getMinutes() - d.getTimezoneOffset());
    return d.toISOString().slice(0, 16);
  };
  $$('input[type="datetime-local"][min]').forEach((input) => {
    if (!input.getAttribute('min')) input.setAttribute('min', nowLocal());
  });

  // Upload
  const data = pageData();
  if (data && data.dataset.page === 'files') {
    const parentId = data.dataset.parentId || '0';
    const uploadUrl = data.dataset.uploadUrl || '/files/upload';
    const fileInput = $('[data-file-input]');
    const folderInput = $('[data-folder-input]');
    const pickFolder = $('[data-pick-folder]');
    const pane = $('#files-pane');
    const panel = $('#upload-panel');
    const counter = $('#upload-count');
    const fileNameEl = $('#upload-file-name');
    const statusEl = $('#upload-status');
    const percentEl = $('#upload-percent');
    const meterEl = $('#upload-meter');
    let uploading = false;

    function showPanel() { if (panel) panel.hidden = false; }
    function setProgress(name, done, total, text) {
      if (fileNameEl) fileNameEl.textContent = name;
      const pct = total > 0 ? Math.min(100, Math.round(done / total * 100)) : 0;
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
      return new Promise((resolve, reject) => {
        const xhr = new XMLHttpRequest();
        xhr.open('POST', uploadUrl);
        xhr.responseType = 'text';
        xhr.upload.onprogress = (event) => {
          if (event.lengthComputable) setProgress(file.name, event.loaded, event.total, '上传中…');
        };
        xhr.onload = () => {
          if (xhr.status >= 200 && xhr.status < 400) resolve();
          else reject(new Error('上传没有完成，请重试'));
        };
        xhr.onerror = () => reject(new Error('网络连接中断，请重试'));
        const form = new FormData();
        form.append('csrf', csrf());
        form.append('parent_id', parentId);
        form.append('path', relDir || '');
        form.append('files', file, file.name);
        xhr.send(form);
      });
    }

    async function initChunked(file, relDir) {
      const body = new URLSearchParams();
      body.set('csrf', csrf());
      body.set('parent_id', parentId);
      body.set('path', relDir || '');
      body.set('name', file.name);
      body.set('size', String(file.size));
      const response = await fetch('/api/upload/init', {
        method: 'POST',
        headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
        body
      });
      if (!response.ok) throw new Error('上传没有开始，请检查网络后重试');
      return await response.json();
    }

    async function chunkStatus(token) {
      const response = await fetch('/api/upload/' + encodeURIComponent(token), { headers: { 'X-CSRF-Token': csrf() } });
      if (!response.ok) return null;
      return await response.json();
    }

    async function putChunk(token, offset, blob) {
      return new Promise((resolve, reject) => {
        const xhr = new XMLHttpRequest();
        xhr.open('POST', '/api/upload/chunk/' + encodeURIComponent(token));
        xhr.setRequestHeader('X-CSRF-Token', csrf());
        xhr.setRequestHeader('X-Offset', String(offset));
        xhr.setRequestHeader('Content-Type', 'application/octet-stream');
        xhr.onload = () => {
          if (xhr.status >= 200 && xhr.status < 300) resolve();
          else reject(new Error('上传中断，请重试'));
        };
        xhr.onerror = () => reject(new Error('网络连接中断，请重试'));
        xhr.send(blob);
      });
    }

    async function uploadChunked(file, relDir) {
      const CHUNK = 4 * 1024 * 1024;
      const fingerprint = 'cn-up-' + [parentId, relDir || '', file.name, file.size, file.lastModified].join('|');
      let token = null, received = 0;
      try { token = localStorage.getItem(fingerprint); } catch (e) {}
      if (token) {
        const status = await chunkStatus(token).catch(() => null);
        if (status && status.size === file.size) {
          received = status.received || 0;
          setProgress(file.name, received, file.size, '继续上传…');
        } else { token = null; try { localStorage.removeItem(fingerprint); } catch (e) {} }
      }
      if (!token) {
        const init = await initChunked(file, relDir);
        token = init.token;
        received = init.received || 0;
        try { localStorage.setItem(fingerprint, token); } catch (e) {}
      }
      while (received < file.size) {
        const end = Math.min(file.size, received + CHUNK);
        const blob = file.slice(received, end);
        let attempts = 0;
        while (true) {
          try { await putChunk(token, received, blob); break; }
          catch (err) {
            attempts++;
            if (attempts >= 4) throw err;
            const status = await chunkStatus(token).catch(() => null);
            if (status) {
              received = status.received || 0;
              if (received >= file.size) break;
            }
            await new Promise((resolve) => setTimeout(resolve, 700 * attempts));
          }
        }
        if (received >= file.size) break;
        received = end;
        setProgress(file.name, received, file.size, '上传中…');
      }
      const response = await fetch('/api/upload/complete/' + encodeURIComponent(token), {
        method: 'POST',
        headers: { 'X-CSRF-Token': csrf() }
      });
      if (!response.ok) throw new Error('上传完成确认失败，请刷新页面检查文件是否已上传');
      try { localStorage.removeItem(fingerprint); } catch (e) {}
    }

    async function uploadBatch(items) {
      if (uploading || !items.length) return;
      uploading = true;
      showPanel();
      if (meterEl) meterEl.style.background = '';
      if (counter) counter.textContent = '（' + items.length + ' 项）';
      let index = 0;
      for (const item of items) {
        index++;
        try {
          if (item.file.size > 16 * 1024 * 1024) await uploadChunked(item.file, item.relDir);
          else await uploadMultipart(item.file, item.relDir);
        } catch (err) {
          fail((err && err.message) || '上传没有完成，请重试');
          uploading = false;
          return;
        }
        if (counter) counter.textContent = '（' + (index + 1) + ' / ' + items.length + '）';
      }
      uploading = false;
      setProgress('上传完成', 1, 1, '全部文件已上传');
      setTimeout(() => { window.location.reload(); }, 450);
    }

    function filesFromList(list, relative) {
      return Array.from(list).map((file) => {
        let relDir = '';
        const relPath = file.webkitRelativePath || '';
        if (relative && relPath) {
          const parts = relPath.split('/');
          parts.pop();
          relDir = parts.join('/');
        }
        return { file, relDir };
      });
    }

    if (fileInput) {
      fileInput.addEventListener('change', () => {
        const items = filesFromList(fileInput.files, false);
        uploadBatch(items);
        fileInput.value = '';
      });
    }
    if (folderInput) {
      folderInput.addEventListener('change', () => {
        const items = filesFromList(folderInput.files, true);
        uploadBatch(items);
        folderInput.value = '';
      });
    }
    if (pickFolder && folderInput) {
      pickFolder.addEventListener('click', () => folderInput.click());
    }

    if (pane) {
      let dragDepth = 0;
      pane.addEventListener('dragenter', (event) => {
        if (!event.dataTransfer || !Array.from(event.dataTransfer.types || []).includes('Files')) return;
        event.preventDefault();
        dragDepth++;
        pane.classList.add('drop-active');
      });
      pane.addEventListener('dragover', (event) => {
        event.preventDefault();
        if (event.dataTransfer) event.dataTransfer.dropEffect = 'copy';
      });
      pane.addEventListener('dragleave', () => {
        dragDepth = Math.max(0, dragDepth - 1);
        if (dragDepth === 0) pane.classList.remove('drop-active');
      });
      pane.addEventListener('drop', (event) => {
        event.preventDefault();
        dragDepth = 0;
        pane.classList.remove('drop-active');
        if (!event.dataTransfer) return;
        const items = filesFromList(event.dataTransfer.files, false);
        uploadBatch(items);
      });
    }
  }
})();
