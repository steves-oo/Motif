import { useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { useItemsStore } from '../stores/itemsStore';
import { useSettingsStore, type Theme, type Lang } from '../stores/settingsStore';
import { STRINGS, type LangStrings } from '../lib/strings';
import { DEFAULT_SHORTCUT, formatAccel, keyEventToAccel } from '../lib/shortcut';
import { isImeComposing } from '../lib/ime';
import { parseCapture } from '../lib/parseCapture';
import type { TagWithCount } from '../types';
import Overview from '../pages/Overview';
import Inbox from '../pages/Inbox';
import Words from '../pages/Words';
import AllItems from '../pages/AllItems';
import { exportAllToMarkdown } from '../lib/exportMarkdown';

function GearIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width="15"
      height="15"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

/** 设置面板中的"快捷捕获快捷键"一行:显示当前组合 + 修改(录制)+ 恢复默认 */
// 开机自启动:状态以系统为准(Windows 注册表 Run 项 / macOS LaunchAgent),不存 config、不同步
function AutostartSetting({ s }: { s: LangStrings }) {
  const [on, setOn] = useState<boolean | null>(null);
  const [err, setErr] = useState(false);

  // 走自实现的命令：Windows 上修复了上游插件注册表路径不加引号的问题
  const queryEnabled = () => invoke<boolean>('is_autostart_enabled');

  useEffect(() => {
    queryEnabled().then(setOn).catch(() => setOn(false));
  }, []);

  const toggle = async (next: boolean) => {
    if (next === on) return;
    setErr(false);
    try {
      await invoke('set_autostart', { enabled: next });
    } catch {
      setErr(true);
    }
    // 无论成败都回读系统真实状态
    setOn(await queryEnabled().catch(() => false));
  };

  const OPTIONS = [
    { value: true, label: s.sidebar_autostart_on },
    { value: false, label: s.sidebar_autostart_off },
  ];

  return (
    <div className="px-3 mb-3">
      <p className="text-xs text-fg-faint mb-1.5">{s.sidebar_autostart_label}</p>
      <div className="flex gap-0.5">
        {OPTIONS.map(({ value, label }) => (
          <button
            key={label}
            onClick={() => toggle(value)}
            disabled={on === null}
            className={`flex-1 text-xs py-1 rounded transition-colors ${
              on === value
                ? 'bg-fg text-canvas font-medium'
                : 'text-fg-muted hover:bg-selected'
            }`}
          >
            {label}
          </button>
        ))}
      </div>
      {err && <p className="text-xs text-danger mt-1">{s.sidebar_autostart_err}</p>}
    </div>
  );
}

function ShortcutSetting({ s }: { s: LangStrings }) {
  const [accel, setAccel] = useState('');
  const [recording, setRecording] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const handlingRef = useRef(false);

  useEffect(() => {
    invoke<string>('get_shortcut').then(setAccel).catch(() => {});
  }, []);

  const apply = async (next: string) => {
    setBusy(true);
    try {
      await invoke('set_shortcut', { accel: next });
      setAccel(next);
      setMsg({ ok: true, text: s.sidebar_shortcut_saved });
    } catch {
      // 注册失败:Rust 侧已回滚旧快捷键,config 未改动
      setMsg({ ok: false, text: s.sidebar_shortcut_err });
    } finally {
      setBusy(false);
    }
    setTimeout(() => setMsg(null), 3000);
  };

  // 录制态:暂停当前全局快捷键,捕获阶段监听按键;退出录制(含面板关闭卸载)时恢复
  useEffect(() => {
    if (!recording) return;
    handlingRef.current = false;
    invoke('pause_shortcut').catch(() => {});

    const onKey = async (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation(); // 不让设置面板的 Esc 关闭、Ctrl+F 等其他快捷键响应
      if (e.repeat || handlingRef.current) return;
      if (e.code === 'Escape') {
        setRecording(false);
        return;
      }
      const next = keyEventToAccel(e);
      if (!next) return; // 组合不完整,继续等待
      handlingRef.current = true;
      await apply(next);
      setRecording(false);
    };
    const onBlur = () => {
      if (!handlingRef.current) setRecording(false);
    };

    window.addEventListener('keydown', onKey, true);
    window.addEventListener('blur', onBlur);
    return () => {
      window.removeEventListener('keydown', onKey, true);
      window.removeEventListener('blur', onBlur);
      invoke('resume_shortcut').catch(() => {});
    };
  }, [recording]);

  const btn =
    'text-xs py-1 px-2 rounded text-fg-muted hover:text-fg-2 hover:bg-selected transition-colors disabled:opacity-40 disabled:hover:bg-transparent';

  return (
    <div className="px-3 mb-3">
      <p className="text-xs text-fg-faint mb-1.5">{s.sidebar_shortcut_label}</p>
      <p className="text-xs text-fg-2 mb-1">{accel ? formatAccel(accel) : '…'}</p>
      {msg && (
        <p
          className={`text-xs mb-1 ${
            msg.ok ? 'text-ok' : 'text-danger'
          }`}
        >
          {msg.text}
        </p>
      )}
      <div className="flex flex-wrap gap-0.5">
        <button
          onClick={(e) => {
            e.currentTarget.blur(); // 避免录制时 Space/Enter 触发按钮本身
            setMsg(null);
            setRecording((v) => !v);
          }}
          disabled={busy}
          className={`${btn} ${recording ? 'bg-selected text-fg' : ''}`}
        >
          {recording ? s.sidebar_shortcut_recording : s.sidebar_shortcut_change}
        </button>
        {!recording && (
          <button
            onClick={() => apply(DEFAULT_SHORTCUT)}
            disabled={busy || accel === DEFAULT_SHORTCUT}
            className={btn}
          >
            {s.sidebar_shortcut_reset}
          </button>
        )}
      </div>
    </div>
  );
}

export default function MainWindow() {
  const {
    currentPage, setPage, inboxCount, words, load, addItem, reloadFromDisk,
    tags, tagFilter, setTagFilter, deleteTag,
    triggerFocusSearch,
  } = useItemsStore();
  const { theme, lang, setTheme, setLang, syncTrayLang } = useSettingsStore();
  const s = STRINGS[lang];

  const [quickInput, setQuickInput] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);
  const [exportMsg, setExportMsg] = useState<string | null>(null);
  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const settingsRef = useRef<HTMLDivElement>(null);
  const [dataDir, setDataDir] = useState('');
  const [dataDirMsg, setDataDirMsg] = useState<string | null>(null);

  useEffect(() => {
    load();
    syncTrayLang();
    invoke<string>('get_data_dir').then(setDataDir).catch(() => {});
  }, []);

  // 捕获窗写文件后广播 item-created；本窗口 repo 缓存独立，需重读磁盘
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    listen('item-created', () => {
      reloadFromDisk();
    }).then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [reloadFromDisk]);

  // 回到主窗口（获得焦点）时重读数据目录，拾取其他设备/同步工具搬来的改动
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (focused) reloadFromDisk();
      })
      .then((fn) => { unlisten = fn; });
    return () => { unlisten?.(); };
  }, [reloadFromDisk]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement;
      const isInput = target.tagName === 'INPUT' || target.tagName === 'TEXTAREA';
      if (((e.ctrlKey || e.metaKey) && e.key === 'f') || (e.key === '/' && !isInput)) {
        e.preventDefault();
        setPage('all');
        triggerFocusSearch();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [setPage, triggerFocusSearch]);

  // 点击设置面板外部关闭
  useEffect(() => {
    if (!isSettingsOpen) return;
    const handleOutside = (e: MouseEvent) => {
      if (settingsRef.current && !settingsRef.current.contains(e.target as Node)) {
        setIsSettingsOpen(false);
      }
    };
    document.addEventListener('mousedown', handleOutside);
    return () => document.removeEventListener('mousedown', handleOutside);
  }, [isSettingsOpen]);

  // Esc 关闭设置面板
  useEffect(() => {
    if (!isSettingsOpen) return;
    const handleEsc = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setIsSettingsOpen(false);
    };
    document.addEventListener('keydown', handleEsc);
    return () => document.removeEventListener('keydown', handleEsc);
  }, [isSettingsOpen]);

  const doQuickAdd = async () => {
    // 与捕获窗同一套语法:`w ` 前缀 → 单词
    const { content, kind } = parseCapture(quickInput);
    if (!content) return;
    await addItem(content, kind);
    setQuickInput('');
    inputRef.current?.focus();
  };

  const handleDeleteTag = async (tag: TagWithCount) => {
    if (tag.count > 0) {
      const ok = window.confirm(s.tag_delete_confirm(tag.count));
      if (!ok) return;
    }
    if (tagFilter === tag.id) setTagFilter(null);
    await deleteTag(tag.id);
  };

  const handleChangeDataDir = async () => {
    const picked = await open({ directory: true, multiple: false, title: s.sidebar_datadir_label });
    if (!picked || Array.isArray(picked)) return;
    await invoke('set_data_dir', { path: picked });
    setDataDir(picked);
    await reloadFromDisk();
    setDataDirMsg(s.sidebar_datadir_changed);
    setTimeout(() => setDataDirMsg(null), 3000);
  };

  const handleExport = async () => {
    try {
      const count = await exportAllToMarkdown();
      if (count > 0) {
        setExportMsg(s.sidebar_export_success(count));
        setTimeout(() => setExportMsg(null), 3000);
      }
    } catch {
      setExportMsg(s.sidebar_export_fail);
      setTimeout(() => setExportMsg(null), 3000);
    }
  };

  const navItems = [
    { key: 'overview' as const, label: s.nav_overview },
    { key: 'inbox' as const, label: `${s.nav_inbox}${inboxCount > 0 ? ` (${inboxCount})` : ''}` },
    { key: 'words' as const, label: `${s.nav_words}${words.length > 0 ? ` (${words.length})` : ''}` },
    { key: 'all' as const, label: s.nav_all },
  ];

  const THEMES: { value: Theme; label: string }[] = [
    { value: 'light', label: s.sidebar_theme_light },
    { value: 'dark', label: s.sidebar_theme_dark },
    { value: 'system', label: s.sidebar_theme_system },
  ];

  const LANGS: { value: Lang; label: string }[] = [
    { value: 'zh', label: '中文' },
    { value: 'en', label: 'EN' },
  ];

  return (
    <div className="flex h-screen bg-canvas text-fg">
      {/* 左侧边栏 */}
      <aside className="w-44 flex-shrink-0 bg-sidebar border-r border-line-soft flex flex-col py-4">
        <div className="px-4 mb-6">
          <span className="text-base font-semibold tracking-tight text-fg">Motif</span>
        </div>

        <nav className="flex-1 px-2 space-y-0.5 overflow-y-auto">
          {navItems.map(({ key, label }) => (
            <button
              key={key}
              onClick={() => {
                if (key === 'all') setTagFilter(null);
                setPage(key);
              }}
              className={`w-full text-left px-3 py-2 rounded-md text-sm transition-colors ${
                currentPage === key && (key !== 'all' || tagFilter === null)
                  ? 'bg-selected text-fg font-medium shadow-[inset_2px_0_0_var(--color-primary)]'
                  : 'text-fg-muted hover:bg-wash hover:text-fg-2'
              }`}
            >
              {label}
            </button>
          ))}

          {tags.length > 0 && (
            <div className="pt-3">
              <p className="text-xs text-fg-faint uppercase tracking-wider px-3 mb-1">
                {s.sidebar_tags}
              </p>
              {tags.map((tag) => (
                <div key={tag.id} className="group relative">
                  <button
                    onClick={() => { setTagFilter(tag.id); setPage('all'); }}
                    className={`w-full text-left px-3 py-1.5 rounded-md text-sm flex items-center transition-colors ${
                      tagFilter === tag.id
                        ? 'bg-selected text-fg font-medium shadow-[inset_2px_0_0_var(--color-primary)]'
                        : 'text-fg-muted hover:bg-wash hover:text-fg-2'
                    }`}
                  >
                    <span className="flex-1 truncate">#{tag.name}</span>
                    <span className="text-xs text-fg-faint ml-1 flex-shrink-0 opacity-100 group-hover:opacity-0 transition-opacity">
                      {tag.count}
                    </span>
                  </button>
                  <button
                    onClick={(e) => { e.stopPropagation(); handleDeleteTag(tag); }}
                    className="absolute right-2 top-1/2 -translate-y-1/2 text-sm text-fg-faint hover:text-danger opacity-0 group-hover:opacity-100 transition-opacity px-1"
                    title={s.tag_delete_title}
                  >
                    ×
                  </button>
                </div>
              ))}
            </div>
          )}
        </nav>

        {/* 底部：设置按钮 + 弹出面板 */}
        <div className="px-2 pt-3 border-t border-line-soft mt-2">
          <div className="relative" ref={settingsRef}>
            {/* 设置弹出面板（向上弹出） */}
            {isSettingsOpen && (
              <div className="absolute bottom-full left-0 mb-1 w-56 bg-surface border border-line rounded-lg shadow-lg dark:shadow-black/40 py-3 z-30">
                {/* 主题 */}
                <div className="px-3 mb-3">
                  <p className="text-xs text-fg-faint mb-1.5">{s.sidebar_theme_label}</p>
                  <div className="flex gap-0.5">
                    {THEMES.map(({ value, label }) => (
                      <button
                        key={value}
                        onClick={() => setTheme(value)}
                        className={`flex-1 text-xs py-1 rounded transition-colors ${
                          theme === value
                            ? 'bg-fg text-canvas font-medium'
                            : 'text-fg-muted hover:bg-selected'
                        }`}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                </div>

                {/* 语言 */}
                <div className="px-3 mb-3">
                  <p className="text-xs text-fg-faint mb-1.5">{s.sidebar_lang_label}</p>
                  <div className="flex gap-0.5">
                    {LANGS.map(({ value, label }) => (
                      <button
                        key={value}
                        onClick={() => setLang(value)}
                        className={`flex-1 text-xs py-1 rounded transition-colors ${
                          lang === value
                            ? 'bg-fg text-canvas font-medium'
                            : 'text-fg-muted hover:bg-selected'
                        }`}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                </div>

                {/* 数据目录 */}
                <div className="px-3 mb-3">
                  <p className="text-xs text-fg-faint mb-1.5">{s.sidebar_datadir_label}</p>
                  <p
                    className="text-xs text-fg-2 break-all leading-snug mb-1"
                    title={dataDir}
                  >
                    {dataDir || '…'}
                  </p>
                  {dataDirMsg && (
                    <p className="text-xs text-ok mb-1">{dataDirMsg}</p>
                  )}
                  <button
                    onClick={handleChangeDataDir}
                    className="text-xs py-1 px-2 rounded text-fg-muted hover:text-fg-2 hover:bg-selected transition-colors"
                  >
                    {s.sidebar_datadir_change}
                  </button>
                </div>

                {/* 快捷捕获快捷键 */}
                <ShortcutSetting s={s} />

                {/* 开机自启动 */}
                <AutostartSetting s={s} />

                {/* 分隔线 */}
                <div className="border-t border-line my-2" />

                {/* 导出 */}
                <div className="px-3">
                  {exportMsg && (
                    <p className="text-xs text-ok mb-1 text-center">{exportMsg}</p>
                  )}
                  <button
                    onClick={handleExport}
                    className="w-full text-left text-xs py-1.5 px-2 rounded text-fg-muted hover:text-fg-2 hover:bg-selected transition-colors"
                  >
                    {s.sidebar_export}
                  </button>
                </div>
              </div>
            )}

            {/* 设置触发按钮 */}
            <button
              onClick={() => setIsSettingsOpen((v) => !v)}
              title={s.settings_label}
              className={`w-full flex items-center gap-2 px-3 py-2 rounded-md text-xs transition-colors ${
                isSettingsOpen
                  ? 'bg-selected text-fg-2'
                  : 'text-fg-faint hover:bg-wash hover:text-fg-muted'
              }`}
            >
              <GearIcon />
              <span>{s.settings_label}</span>
            </button>
          </div>
        </div>
      </aside>

      {/* 右侧内容区 */}
      <main className="flex-1 flex flex-col overflow-hidden">
        <form
          onSubmit={(e) => { e.preventDefault(); doQuickAdd(); }}
          className="px-6 py-3 border-b border-line-soft flex gap-2"
        >
          <input
            ref={inputRef}
            value={quickInput}
            onChange={(e) => setQuickInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                if (!isImeComposing(e)) doQuickAdd();
              }
            }}
            placeholder={s.quickadd_placeholder}
            className="flex-1 text-sm border border-line rounded-md px-3 py-1.5 bg-surface text-fg focus:outline-none focus:border-line-strong placeholder:text-fg-ghost"
          />
          <button
            type="submit"
            disabled={!quickInput.trim()}
            className="text-sm px-3 py-1.5 rounded-md bg-primary text-primary-fg disabled:opacity-30 hover:bg-primary/85 transition-colors"
          >
            {s.quickadd_button}
          </button>
        </form>

        <div className="flex-1 overflow-y-auto">
          {currentPage === 'overview' && <Overview />}
          {currentPage === 'inbox' && <Inbox />}
          {currentPage === 'words' && <Words />}
          {currentPage === 'all' && <AllItems />}
        </div>
      </main>
    </div>
  );
}
