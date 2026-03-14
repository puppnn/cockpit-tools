/**
 * useProviderAccountsPage
 *
 * 通用 hook：封装所有 Provider AccountsPage（Kiro / Windsurf / GitHubCopilot / Codex）
 * 共享的 state、effects 和 handlers。
 *
 * 各平台页面只需提供一个 ProviderPageConfig 即可复用全部通用逻辑。
 */

import {
  useState,
  useEffect,
  useRef,
  useMemo,
  useCallback,
  type RefObject,
  type Dispatch,
  type SetStateAction,
} from 'react';
import { useTranslation } from 'react-i18next';
import { openUrl } from '@tauri-apps/plugin-opener';
import {
  isPrivacyModeEnabledByDefault,
  maskSensitiveValue,
  persistPrivacyModeEnabled,
} from '../utils/privacy';
import { useExportJsonModal } from './useExportJsonModal';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export type AddModalStatus = 'idle' | 'loading' | 'success' | 'error';
export type ViewMode = 'grid' | 'list';
export type SortDirection = 'asc' | 'desc';

/** 各平台需要提供的 OAuth 服务函数 */
export interface OAuthService {
  startLogin: () => Promise<OAuthStartResponse>;
  completeLogin: (loginId: string) => Promise<unknown>;
  cancelLogin: (loginId?: string) => Promise<void>;
  submitCallbackUrl?: (loginId: string, callbackUrl: string) => Promise<void>;
  openAuthUrl?: (url: string) => Promise<void>;
}

export interface OAuthStartResponse {
  loginId: string;
  userCode?: string;
  verificationUri?: string;
  verificationUriComplete?: string | null;
  expiresIn: number;
  intervalSeconds: number;
  callbackUrl?: string | null;
  /** Codex 模式使用 authUrl 而非 verificationUri */
  authUrl?: string;
}

/** 各平台需要提供的数据服务函数 */
export interface ProviderDataService {
  importFromJson: (content: string) => Promise<unknown[]>;
  importFromLocal?: () => Promise<unknown[]>;
  addWithToken?: (token: string) => Promise<unknown>;
  exportAccounts: (ids: string[]) => Promise<string>;
  injectToVSCode?: (accountId: string) => Promise<unknown>;
}

/** 各平台 store 需要提供的操作 */
export interface ProviderStoreActions<TAccount> {
  accounts: TAccount[];
  loading: boolean;
  fetchAccounts: () => Promise<void>;
  deleteAccounts: (ids: string[]) => Promise<void>;
  refreshToken: (id: string) => Promise<void>;
  refreshAllTokens: () => Promise<void>;
  updateAccountTags: (id: string, tags: string[]) => Promise<unknown>;
}

/** 配置对象：各平台页面的差异化配置 */
export interface ProviderPageConfig<TAccount extends ProviderAccountBase> {
  /** 平台标识，用于日志和 localStorage key */
  platformKey: string;
  /** OAuth 日志前缀 */
  oauthLogPrefix: string;
  /** localStorage key：flow notice 折叠状态 */
  flowNoticeCollapsedKey?: string;
  /** localStorage key：当前选中账号 */
  currentAccountIdKey?: string;
  /** 导出文件名前缀 */
  exportFilePrefix: string;
  /** Store 操作 */
  store: ProviderStoreActions<TAccount>;
  /** OAuth 服务（可选，Codex 等使用自定义 OAuth 流程的平台可不传） */
  oauthService?: OAuthService;
  /** 触发 OAuth 流程的 addTab key，默认 ['oauth'] */
  oauthTabKeys?: string[];
  /** 数据服务 */
  dataService: ProviderDataService;
  /** 获取展示用 email/displayName */
  getDisplayEmail: (account: TAccount) => string;
  /** 切号注入成功后的扩展回调（可选） */
  onInjectSuccess?: (params: {
    accountId: string;
    account: TAccount | undefined;
    displayEmail: string;
  }) => void | Promise<void>;
}

export interface ProviderAccountBase {
  id: string;
  created_at: number;
  tags?: string[] | null;
}

const DEFAULT_SORT_BY = 'created_at';
const DEFAULT_SORT_DIRECTION: SortDirection = 'desc';

const normalizeSortDirection = (value: string | null): SortDirection =>
  value === 'asc' ? 'asc' : DEFAULT_SORT_DIRECTION;

const buildSortStorageKeys = (platformKey: string) => {
  const scope = platformKey.trim().toLowerCase().replace(/[^a-z0-9]+/g, '_');
  return {
    sortByKey: `agtools.${scope}.accounts_sort_by`,
    sortDirectionKey: `agtools.${scope}.accounts_sort_direction`,
  };
};

// ---------------------------------------------------------------------------
// Hook return type
// ---------------------------------------------------------------------------

export interface UseProviderAccountsPageReturn {
  // i18n
  t: ReturnType<typeof useTranslation>['t'];
  locale: string;

  // Privacy
  privacyModeEnabled: boolean;
  togglePrivacyMode: () => void;
  maskAccountText: (value?: string | null) => string;

  // View mode
  viewMode: ViewMode;
  setViewMode: (mode: ViewMode) => void;

  // Search & Filter
  searchQuery: string;
  setSearchQuery: (q: string) => void;
  filterType: string;
  setFilterType: (type: string) => void;

  // Sort
  sortBy: string;
  setSortBy: (sort: string) => void;
  sortDirection: SortDirection;
  setSortDirection: Dispatch<SetStateAction<SortDirection>>;

  // Selection
  selected: Set<string>;
  setSelected: (s: Set<string>) => void;
  toggleSelect: (id: string) => void;
  toggleSelectAll: (filteredIds: string[]) => void;

  // Tags
  tagFilter: string[];
  setTagFilter: (tags: string[]) => void;
  groupByTag: boolean;
  setGroupByTag: (v: boolean) => void;
  showTagFilter: boolean;
  setShowTagFilter: Dispatch<SetStateAction<boolean>>;
  showTagModal: string | null;
  setShowTagModal: (id: string | null) => void;
  tagFilterRef: RefObject<HTMLDivElement | null>;
  availableTags: string[];
  toggleTagFilterValue: (tag: string) => void;
  clearTagFilter: () => void;
  tagDeleteConfirm: { tag: string; count: number } | null;
  setTagDeleteConfirm: (v: { tag: string; count: number } | null) => void;
  deletingTag: boolean;
  requestDeleteTag: (tag: string) => void;
  confirmDeleteTag: () => Promise<void>;
  openTagModal: (accountId: string) => void;
  handleSaveTags: (tags: string[]) => Promise<void>;

  // CRUD
  refreshing: string | null;
  refreshingAll: boolean;
  injecting: string | null;
  handleRefresh: (accountId: string) => Promise<void>;
  handleRefreshAll: () => Promise<void>;
  handleDelete: (accountId: string) => void;
  handleBatchDelete: () => void;
  deleteConfirm: { ids: string[]; message: string } | null;
  setDeleteConfirm: (v: { ids: string[]; message: string } | null) => void;
  deleting: boolean;
  confirmDelete: () => Promise<void>;

  // Messages
  message: { text: string; tone?: 'error' } | null;
  setMessage: (msg: { text: string; tone?: 'error' } | null) => void;

  // Export
  exporting: boolean;
  handleExport: () => Promise<void>;
  handleExportByIds: (ids: string[], fileNameBase?: string) => Promise<void>;
  showExportModal: boolean;
  closeExportModal: () => void;
  exportJsonContent: string;
  exportJsonHidden: boolean;
  toggleExportJsonHidden: () => void;
  exportJsonCopied: boolean;
  copyExportJson: () => Promise<void>;
  savingExportJson: boolean;
  saveExportJson: () => Promise<void>;
  exportSavedPath: string | null;
  canOpenExportSavedDirectory: boolean;
  openExportSavedDirectory: () => Promise<void>;
  copyExportSavedPath: () => Promise<void>;
  exportPathCopied: boolean;

  // Add modal
  showAddModal: boolean;
  setShowAddModal: (v: boolean) => void;
  addTab: string;
  setAddTab: (tab: string) => void;
  addStatus: AddModalStatus;
  setAddStatus: (s: AddModalStatus) => void;
  addMessage: string | null;
  setAddMessage: (msg: string | null) => void;
  tokenInput: string;
  setTokenInput: (v: string) => void;
  importing: boolean;
  openAddModal: (tab: string) => void;
  closeAddModal: () => void;
  resetAddModalState: () => void;
  handleTokenImport: () => Promise<void>;
  handleImportJsonFile: (file: File) => Promise<void>;
  handleImportFromLocal: (() => Promise<void>) | null;
  handlePickImportFile: () => void;
  importFileInputRef: RefObject<HTMLInputElement | null>;

  // OAuth (device flow style: Kiro / Windsurf / GHCP)
  oauthUrl: string | null;
  oauthCallbackUrl: string | null;
  oauthUrlCopied: boolean;
  oauthUserCode: string | null;
  oauthUserCodeCopied: boolean;
  oauthMeta: { expiresIn: number; intervalSeconds: number } | null;
  oauthPrepareError: string | null;
  oauthCompleteError: string | null;
  oauthPolling: boolean;
  oauthTimedOut: boolean;
  oauthManualCallbackInput: string;
  setOauthManualCallbackInput: (value: string) => void;
  oauthManualCallbackSubmitting: boolean;
  oauthManualCallbackError: string | null;
  oauthSupportsManualCallback: boolean;
  handleCopyOauthUrl: () => Promise<void>;
  handleCopyOauthUserCode: () => Promise<void>;
  handleRetryOauth: () => void;
  handleRetryOauthComplete: () => void;
  handleOpenOauthUrl: () => Promise<void>;
  handleSubmitOauthCallbackUrl: () => Promise<void>;

  // Inject / Switch
  handleInjectToVSCode: ((accountId: string) => Promise<void>) | null;

  // Flow notice
  isFlowNoticeCollapsed: boolean;
  setIsFlowNoticeCollapsed: Dispatch<SetStateAction<boolean>>;

  // Current account
  currentAccountId: string | null;
  setCurrentAccountId: (id: string | null) => void;

  // Utilities
  formatDate: (timestamp: number) => string;
  normalizeTag: (tag: string) => string;
  resolveDefaultExportPath: (fileName: string) => Promise<string>;
  saveJsonFile: (json: string, defaultFileName: string) => Promise<string | null>;
}

// ---------------------------------------------------------------------------
// Hook implementation
// ---------------------------------------------------------------------------

export function useProviderAccountsPage<TAccount extends ProviderAccountBase>(
  config: ProviderPageConfig<TAccount>,
): UseProviderAccountsPageReturn {
  const { t, i18n } = useTranslation();
  const locale = i18n.language || 'zh-CN';


  const {
    platformKey,
    oauthLogPrefix,
    flowNoticeCollapsedKey,
    currentAccountIdKey,
    exportFilePrefix,
    store,
    oauthService,
    oauthTabKeys: oauthTabKeysConfig,
    dataService,
  } = config;

  const oauthTabKeys = useMemo(() => {
    const normalized = (oauthTabKeysConfig || [])
      .map((item) => item.trim())
      .filter(Boolean);
    return normalized.length > 0 ? normalized : ['oauth'];
  }, [oauthTabKeysConfig]);

  const {
    accounts,
    fetchAccounts,
    deleteAccounts,
    refreshToken,
    refreshAllTokens,
    updateAccountTags,
  } = store;
  const { sortByKey, sortDirectionKey } = buildSortStorageKeys(platformKey);

  // ─── Privacy ──────────────────────────────────────────────────────────
  const [privacyModeEnabled, setPrivacyModeEnabled] = useState<boolean>(() =>
    isPrivacyModeEnabledByDefault(),
  );

  const togglePrivacyMode = useCallback(() => {
    setPrivacyModeEnabled((prev) => {
      const next = !prev;
      persistPrivacyModeEnabled(next);
      return next;
    });
  }, []);

  const maskAccountText = useCallback(
    (value?: string | null) => maskSensitiveValue(value, privacyModeEnabled),
    [privacyModeEnabled],
  );

  // ─── View Mode ────────────────────────────────────────────────────────
  const [viewMode, setViewMode] = useState<ViewMode>('grid');

  // ─── Search & Filter ──────────────────────────────────────────────────
  const [searchQuery, setSearchQuery] = useState('');
  const [filterType, setFilterType] = useState<string>('all');

  // ─── Sort ─────────────────────────────────────────────────────────────
  const [sortBy, setSortBy] = useState<string>(() => {
    const saved = localStorage.getItem(sortByKey);
    return saved?.trim() ? saved : DEFAULT_SORT_BY;
  });
  const [sortDirection, setSortDirection] = useState<SortDirection>(() =>
    normalizeSortDirection(localStorage.getItem(sortDirectionKey)),
  );

  useEffect(() => {
    localStorage.setItem(sortByKey, sortBy);
  }, [sortBy, sortByKey]);

  useEffect(() => {
    localStorage.setItem(sortDirectionKey, sortDirection);
  }, [sortDirection, sortDirectionKey]);

  // ─── Selection ────────────────────────────────────────────────────────
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const toggleSelect = useCallback((id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const toggleSelectAll = useCallback(
    (filteredIds: string[]) => {
      const allSelected = selected.size === filteredIds.length && filteredIds.length > 0;
      setSelected(allSelected ? new Set() : new Set(filteredIds));
    },
    [selected.size],
  );

  // ─── Tags ─────────────────────────────────────────────────────────────
  const [tagFilter, setTagFilter] = useState<string[]>([]);
  const [groupByTag, setGroupByTag] = useState(false);
  const [showTagFilter, setShowTagFilter] = useState(false);
  const [showTagModal, setShowTagModal] = useState<string | null>(null);
  const [tagDeleteConfirm, setTagDeleteConfirm] = useState<{
    tag: string;
    count: number;
  } | null>(null);
  const [deletingTag, setDeletingTag] = useState(false);
  const tagFilterRef = useRef<HTMLDivElement | null>(null);

  const normalizeTag = useCallback((tag: string) => tag.trim().toLowerCase(), []);

  const availableTags = useMemo(() => {
    const set = new Set<string>();
    accounts.forEach((account) => {
      (account.tags || []).forEach((tag) => {
        const normalized = normalizeTag(tag);
        if (normalized) set.add(normalized);
      });
    });
    return Array.from(set).sort((a, b) => a.localeCompare(b));
  }, [accounts, normalizeTag]);

  const toggleTagFilterValue = useCallback((tag: string) => {
    setTagFilter((prev) => {
      if (prev.includes(tag)) return prev.filter((item) => item !== tag);
      return [...prev, tag];
    });
  }, []);

  const clearTagFilter = useCallback(() => {
    setTagFilter([]);
  }, []);

  const requestDeleteTag = useCallback(
    (tag: string) => {
      const normalized = normalizeTag(tag);
      if (!normalized) return;
      const count = accounts.filter((acc) =>
        (acc.tags || []).some((t) => normalizeTag(t) === normalized),
      ).length;
      setTagDeleteConfirm({ tag: normalized, count });
    },
    [accounts, normalizeTag],
  );

  const confirmDeleteTag = useCallback(async () => {
    if (!tagDeleteConfirm || deletingTag) return;
    setDeletingTag(true);
    const target = tagDeleteConfirm.tag;
    try {
      const affectedAccounts = accounts.filter((acc) =>
        (acc.tags || []).some((t) => normalizeTag(t) === target),
      );
      for (const acc of affectedAccounts) {
        const newTags = (acc.tags || []).filter((t) => normalizeTag(t) !== target);
        await updateAccountTags(acc.id, newTags);
      }
      setTagFilter((prev) => prev.filter((t) => normalizeTag(t) !== target));
      setTagDeleteConfirm(null);
    } finally {
      setDeletingTag(false);
    }
  }, [tagDeleteConfirm, deletingTag, accounts, normalizeTag, updateAccountTags]);

  const openTagModal = useCallback((accountId: string) => {
    setShowTagModal(accountId);
  }, []);

  const handleSaveTags = useCallback(
    async (tags: string[]) => {
      if (!showTagModal) return;
      await updateAccountTags(showTagModal, tags);
      setShowTagModal(null);
    },
    [showTagModal, updateAccountTags],
  );

  // ─── Tag filter click-outside ─────────────────────────────────────────
  useEffect(() => {
    if (!showTagFilter) return;
    const handleClick = (event: MouseEvent) => {
      if (!tagFilterRef.current) return;
      if (!tagFilterRef.current.contains(event.target as Node)) {
        setShowTagFilter(false);
      }
    };
    document.addEventListener('mousedown', handleClick);
    return () => document.removeEventListener('mousedown', handleClick);
  }, [showTagFilter]);

  // ─── Fetch on mount ───────────────────────────────────────────────────
  useEffect(() => {
    fetchAccounts();
  }, [fetchAccounts]);

  // ─── CRUD ─────────────────────────────────────────────────────────────
  const [refreshing, setRefreshing] = useState<string | null>(null);
  const [refreshingAll, setRefreshingAll] = useState(false);
  const [injecting, setInjecting] = useState<string | null>(null);
  const [deleteConfirm, setDeleteConfirm] = useState<{
    ids: string[];
    message: string;
  } | null>(null);
  const [deleting, setDeleting] = useState(false);
  const [message, setMessage] = useState<{ text: string; tone?: 'error' } | null>(null);

  const handleRefresh = useCallback(
    async (accountId: string) => {
      setRefreshing(accountId);
      try {
        await refreshToken(accountId);
      } catch (e) {
        console.error(e);
      }
      setRefreshing(null);
    },
    [refreshToken],
  );

  const handleRefreshAll = useCallback(async () => {
    setRefreshingAll(true);
    try {
      await refreshAllTokens();
    } catch (e) {
      console.error(e);
    }
    setRefreshingAll(false);
  }, [refreshAllTokens]);

  const handleDelete = useCallback(
    (accountId: string) => {
      setDeleteConfirm({
        ids: [accountId],
        message: t('messages.deleteConfirm', '确定要删除此账号吗？'),
      });
    },
    [t],
  );

  const handleBatchDelete = useCallback(() => {
    if (selected.size === 0) return;
    setDeleteConfirm({
      ids: Array.from(selected),
      message: t('messages.batchDeleteConfirm', { count: selected.size }),
    });
  }, [selected, t]);

  const confirmDelete = useCallback(async () => {
    if (!deleteConfirm || deleting) return;
    setDeleting(true);
    try {
      await deleteAccounts(deleteConfirm.ids);
      setSelected((prev) => {
        const next = new Set(prev);
        deleteConfirm.ids.forEach((id) => next.delete(id));
        return next;
      });
      setDeleteConfirm(null);
    } finally {
      setDeleting(false);
    }
  }, [deleteConfirm, deleting, deleteAccounts]);

  // ─── Inject ───────────────────────────────────────────────────────────
  const handleInjectToVSCode = useMemo(() => {
    if (!dataService.injectToVSCode) return null;
    const injectFn = dataService.injectToVSCode;
    return async (accountId: string) => {
      setMessage(null);
      setInjecting(accountId);
      const account = accounts.find((item) => item.id === accountId);
      const displayEmail = account ? config.getDisplayEmail(account) : accountId;
      try {
        await injectFn(accountId);
        setCurrentAccountId(accountId);
        setMessage({ text: t('messages.switched', { email: maskAccountText(displayEmail) }) });
        if (config.onInjectSuccess) {
          try {
            await config.onInjectSuccess({
              accountId,
              account,
              displayEmail,
            });
          } catch (callbackError) {
            console.error(`[${platformKey}] onInjectSuccess callback failed:`, callbackError);
          }
        }
      } catch (e: unknown) {
        setMessage({
          text: t('messages.switchFailed', {
            error: String(e) || t('common.failed', 'Failed'),
          }),
          tone: 'error',
        });
      }
      setInjecting(null);
    };
  }, [dataService.injectToVSCode, accounts, config, t, maskAccountText, platformKey]);

  // ─── Export ───────────────────────────────────────────────────────────
  const handleExportError = useCallback(
    (error: unknown) => {
      setMessage({
        text: t('messages.exportFailed', { error: String(error) }),
        tone: 'error',
      });
    },
    [t],
  );

  const exportModal = useExportJsonModal({
    exportFilePrefix,
    exportJsonByIds: dataService.exportAccounts,
    onError: handleExportError,
  });

  const handleExportByIds = useCallback(
    async (ids: string[], fileNameBase?: string) => {
      if (!ids.length) return;
      await exportModal.startExport(ids, fileNameBase);
    },
    [exportModal.startExport],
  );

  const handleExport = useCallback(async () => {
    try {
      const ids = selected.size > 0 ? Array.from(selected) : accounts.map((a) => a.id);
      await handleExportByIds(ids);
    } catch (error) {
      handleExportError(error);
    }
  }, [selected, accounts, handleExportByIds, handleExportError]);

  const exporting = exportModal.preparing;

  // ─── Add Modal ────────────────────────────────────────────────────────
  const [showAddModal, setShowAddModal] = useState(false);
  const [addTab, setAddTab] = useState<string>('oauth');
  const [addStatus, setAddStatus] = useState<AddModalStatus>('idle');
  const [addMessage, setAddMessage] = useState<string | null>(null);
  const [tokenInput, setTokenInput] = useState('');
  const [importing, setImporting] = useState(false);

  const showAddModalRef = useRef(showAddModal);
  const addTabRef = useRef(addTab);
  const addStatusRef = useRef(addStatus);
  const importFileInputRef = useRef<HTMLInputElement | null>(null);
  const oauthServiceRef = useRef(oauthService);

  useEffect(() => {
    showAddModalRef.current = showAddModal;
    addTabRef.current = addTab;
    addStatusRef.current = addStatus;
    oauthServiceRef.current = oauthService;
  }, [showAddModal, addTab, addStatus, oauthService]);

  const resetAddModalState = useCallback(() => {
    oauthAttemptSeqRef.current += 1;
    setAddStatus('idle');
    setAddMessage('');
    setTokenInput('');
    setOauthUrl(null);
    setOauthCallbackUrl(null);
    setOauthUrlCopied(false);
    setOauthUserCode(null);
    setOauthUserCodeCopied(false);
    setOauthMeta(null);
    setOauthPrepareError(null);
    setOauthCompleteError(null);
    setOauthTimedOut(false);
    setOauthPolling(false);
    setOauthManualCallbackInput('');
    setOauthManualCallbackSubmitting(false);
    setOauthManualCallbackError(null);
    oauthActiveRef.current = false;
    oauthCompletingRef.current = false;
    oauthLoginIdRef.current = null;
  }, []);

  const openAddModal = useCallback(
    (tab: string) => {
      setAddTab(tab);
      setShowAddModal(true);
      resetAddModalState();
    },
    [resetAddModalState],
  );

  const closeAddModal = useCallback(() => {
    setShowAddModal(false);
    resetAddModalState();
  }, [resetAddModalState]);

  const handlePickImportFile = useCallback(() => {
    importFileInputRef.current?.click();
  }, []);

  // ─── Import ───────────────────────────────────────────────────────────
  const handleImportJsonFile = useCallback(
    async (file: File) => {
      setImporting(true);
      setAddStatus('loading');
      setAddMessage(t('common.shared.import.importing', '正在导入...'));

      try {
        const content = await file.text();
        const imported = await dataService.importFromJson(content);
        await fetchAccounts();

        setAddStatus('success');
        setAddMessage(
          t('common.shared.token.importSuccessMsg', {
            count: imported.length,
            defaultValue: '成功导入 {{count}} 个账号',
          }),
        );
        setTimeout(() => {
          setShowAddModal(false);
          resetAddModalState();
        }, 1200);
      } catch (e) {
        setAddStatus('error');
        const errorMsg = String(e).replace(/^Error:\s*/, '');
        setAddMessage(
          t('common.shared.import.failedMsg', {
            error: errorMsg,
            defaultValue: '导入失败: {{error}}',
          }),
        );
      }

      setImporting(false);
    },
    [dataService, fetchAccounts, resetAddModalState, t],
  );

  const handleImportFromLocal = useMemo(() => {
    if (!dataService.importFromLocal) return null;
    const importFn = dataService.importFromLocal;
    return async () => {
      setImporting(true);
      setAddStatus('loading');
      setAddMessage(t('common.shared.import.importing', '正在导入...'));
      try {
        const imported = await importFn();
        await fetchAccounts();
        setAddStatus('success');
        setAddMessage(
          t('common.shared.token.importSuccessMsg', {
            count: imported.length,
            defaultValue: '成功导入 {{count}} 个账号',
          }),
        );
        setTimeout(() => {
          setShowAddModal(false);
          resetAddModalState();
        }, 1200);
      } catch (e) {
        setAddStatus('error');
        const errorMsg = String(e).replace(/^Error:\s*/, '');
        setAddMessage(
          t('common.shared.import.failedMsg', {
            error: errorMsg,
            defaultValue: '导入失败: {{error}}',
          }),
        );
      }
      setImporting(false);
    };
  }, [dataService.importFromLocal, fetchAccounts, resetAddModalState, t]);

  const handleTokenImport = useCallback(async () => {
    const trimmed = tokenInput.trim();
    if (!trimmed) {
      setAddStatus('error');
      setAddMessage(t('common.shared.token.empty', '请输入 Token 或 JSON'));
      return;
    }

    setImporting(true);
    setAddStatus('loading');
    setAddMessage(t('common.shared.token.importing', '正在导入...'));

    try {
      let importedCount = 0;
      if (trimmed.startsWith('{') || trimmed.startsWith('[')) {
        const imported = await dataService.importFromJson(trimmed);
        importedCount = imported.length;
      } else if (dataService.addWithToken) {
        await dataService.addWithToken(trimmed);
        importedCount = 1;
      } else {
        const imported = await dataService.importFromJson(trimmed);
        importedCount = imported.length;
      }
      await fetchAccounts();
      setAddStatus('success');
      setAddMessage(
        t('common.shared.token.importSuccessMsg', {
          count: importedCount,
          defaultValue: '成功导入 {{count}} 个账号',
        }),
      );
      setTimeout(() => {
        setShowAddModal(false);
        resetAddModalState();
      }, 1200);
    } catch (e) {
      setAddStatus('error');
      const errorMsg = String(e).replace(/^Error:\s*/, '');
      setAddMessage(
        t('common.shared.token.importFailedMsg', {
          error: errorMsg,
          defaultValue: '导入失败: {{error}}',
        }),
      );
    }
    setImporting(false);
  }, [tokenInput, dataService, fetchAccounts, resetAddModalState, t]);

  // ─── OAuth (Device Flow) ──────────────────────────────────────────────
  const [oauthUrl, setOauthUrl] = useState<string | null>(null);
  const [oauthCallbackUrl, setOauthCallbackUrl] = useState<string | null>(null);
  const [oauthUrlCopied, setOauthUrlCopied] = useState(false);
  const [oauthUserCode, setOauthUserCode] = useState<string | null>(null);
  const [oauthUserCodeCopied, setOauthUserCodeCopied] = useState(false);
  const [oauthMeta, setOauthMeta] = useState<{
    expiresIn: number;
    intervalSeconds: number;
  } | null>(null);
  const [oauthPrepareError, setOauthPrepareError] = useState<string | null>(null);
  const [oauthCompleteError, setOauthCompleteError] = useState<string | null>(null);
  const [oauthPolling, setOauthPolling] = useState(false);
  const [oauthTimedOut, setOauthTimedOut] = useState(false);
  const [oauthManualCallbackInput, setOauthManualCallbackInput] = useState('');
  const [oauthManualCallbackSubmitting, setOauthManualCallbackSubmitting] = useState(false);
  const [oauthManualCallbackError, setOauthManualCallbackError] = useState<string | null>(null);

  const oauthActiveRef = useRef(false);
  const oauthLoginIdRef = useRef<string | null>(null);
  const oauthCompletingRef = useRef(false);
  const oauthAttemptSeqRef = useRef(0);

  const oauthLog = useCallback(
    (...args: unknown[]) => {
      console.info(`[${oauthLogPrefix}]`, ...args);
    },
    [oauthLogPrefix],
  );

  const handleOauthPrepareError = useCallback(
    (e: unknown) => {
      const msg = String(e).replace(/^Error:\s*/, '');
      console.error(`[${oauthLogPrefix}] 准备授权信息失败`, { error: msg });
      oauthActiveRef.current = false;
      oauthCompletingRef.current = false;
      setOauthPolling(false);
      setOauthCallbackUrl(null);
      setOauthManualCallbackSubmitting(false);
      setOauthManualCallbackError(null);
      setOauthPrepareError(t('common.shared.oauth.failed', '授权失败') + ': ' + msg);
    },
    [oauthLogPrefix, t],
  );

  const completeOauthSuccess = useCallback(async () => {
    oauthLog('授权完成并保存成功', {
      loginId: oauthLoginIdRef.current,
    });
    await fetchAccounts();
    setAddStatus('success');
    setAddMessage(t('common.shared.oauth.success', '授权成功'));
    // 授权完成后不再触发 cancelLogin，避免误关仍需用户手动确认的授权页
    oauthLoginIdRef.current = null;
    oauthActiveRef.current = false;
    oauthCompletingRef.current = false;
    setOauthUrl(null);
    setOauthCallbackUrl(null);
    setOauthUrlCopied(false);
    setOauthUserCode(null);
    setOauthUserCodeCopied(false);
    setOauthMeta(null);
    setOauthPrepareError(null);
    setOauthCompleteError(null);
    setOauthTimedOut(false);
    setOauthPolling(false);
    setOauthManualCallbackSubmitting(false);
    setOauthManualCallbackError(null);
    setTimeout(() => {
      setShowAddModal(false);
      resetAddModalState();
    }, 1200);
  }, [fetchAccounts, t, oauthLog, resetAddModalState]);

  const handleOauthCompleteError = useCallback(
    (e: unknown) => {
      const msg = String(e).replace(/^Error:\s*/, '');
      setOauthCompleteError(msg);
      setOauthTimedOut(/超时|过期|expired|timeout/i.test(msg));
      setOauthPolling(false);
      setOauthManualCallbackSubmitting(false);
      oauthCompletingRef.current = false;
      oauthActiveRef.current = false;
      oauthLog(`${platformKey} OAuth 授权失败`, {
        loginId: oauthLoginIdRef.current,
        error: msg,
      });
    },
    [oauthLog, platformKey],
  );

  const prepareOauthUrl = useCallback(() => {
    if (!oauthService) return;
    if (!showAddModalRef.current || !oauthTabKeys.includes(addTabRef.current)) return;
    if (oauthActiveRef.current) return;
    if (oauthCompletingRef.current) return;
    const attemptSeq = ++oauthAttemptSeqRef.current;
    oauthActiveRef.current = true;
    setOauthPrepareError(null);
    setOauthCompleteError(null);
    setOauthTimedOut(false);
    setOauthPolling(false);
    setOauthUrlCopied(false);
    setOauthUserCodeCopied(false);
    setOauthMeta(null);
    setOauthUserCode(null);
    setOauthCallbackUrl(null);
    setOauthManualCallbackSubmitting(false);
    setOauthManualCallbackError(null);
    setOauthManualCallbackInput('');
    oauthLog(`开始准备 ${platformKey} OAuth 授权信息`);

    let started = false;

    void (async () => {
      try {
        const resp = await oauthService.startLogin();
        started = true;

        if (attemptSeq !== oauthAttemptSeqRef.current) {
          oauthService.cancelLogin(resp.loginId).catch(() => {});
          oauthLog('忽略过期 OAuth start 响应', { attemptSeq, loginId: resp.loginId });
          return;
        }

        oauthLoginIdRef.current = resp.loginId ?? null;

        const url =
          resp.verificationUriComplete || resp.verificationUri || resp.authUrl || '';
        setOauthUrl(url);
        setOauthCallbackUrl(resp.callbackUrl ?? null);
        setOauthUserCode(resp.userCode ?? null);
        if (resp.expiresIn || resp.intervalSeconds) {
          setOauthMeta({
            expiresIn: resp.expiresIn,
            intervalSeconds: resp.intervalSeconds,
          });
        }

        oauthLog('授权信息已就绪并展示在弹框', {
          loginId: resp.loginId,
          url,
          expiresIn: resp.expiresIn,
          intervalSeconds: resp.intervalSeconds,
          attemptSeq,
        });

        setOauthPolling(true);
        oauthCompletingRef.current = true;
        oauthActiveRef.current = false;
        await oauthService.completeLogin(resp.loginId);

        if (attemptSeq !== oauthAttemptSeqRef.current) {
          oauthLog('忽略过期 OAuth complete 成功回调', {
            attemptSeq,
            loginId: resp.loginId,
          });
          return;
        }

        setOauthPolling(false);
        oauthCompletingRef.current = false;
        await completeOauthSuccess();
      } catch (e) {
        if (attemptSeq !== oauthAttemptSeqRef.current) {
          oauthLog('忽略过期 OAuth 异常回调', {
            attemptSeq,
            error: String(e),
          });
          return;
        }
        if (!started) {
          handleOauthPrepareError(e);
          return;
        }
        handleOauthCompleteError(e);
      } finally {
        if (attemptSeq === oauthAttemptSeqRef.current) {
          oauthActiveRef.current = false;
        }
      }
    })();
  }, [
    oauthService,
    completeOauthSuccess,
    handleOauthCompleteError,
    handleOauthPrepareError,
    oauthLog,
    oauthTabKeys,
    platformKey,
  ]);

  // Auto-prepare OAuth when modal opens on oauth tab
  useEffect(() => {
    if (!showAddModal || !oauthTabKeys.includes(addTab) || oauthUrl) return;
    prepareOauthUrl();
  }, [showAddModal, addTab, oauthUrl, prepareOauthUrl, oauthTabKeys]);

  // Cancel OAuth when modal closes or tab changes
  useEffect(() => {
    if (showAddModal && oauthTabKeys.includes(addTab)) return;
    const loginId = oauthLoginIdRef.current ?? undefined;
    const hasOauthUiResidue = Boolean(oauthUrl)
      || Boolean(oauthCallbackUrl)
      || oauthUrlCopied
      || Boolean(oauthUserCode)
      || oauthUserCodeCopied
      || oauthMeta !== null
      || Boolean(oauthPrepareError)
      || Boolean(oauthCompleteError)
      || oauthTimedOut
      || oauthPolling
      || oauthManualCallbackInput.length > 0
      || oauthManualCallbackSubmitting
      || Boolean(oauthManualCallbackError);
    if (!loginId && !oauthActiveRef.current && !oauthCompletingRef.current && !hasOauthUiResidue) return;
    oauthAttemptSeqRef.current += 1;
    if (loginId) {
      oauthLog('弹框关闭或切换标签，准备取消授权流程', { loginId });
      oauthService?.cancelLogin(loginId).catch(() => {});
    }
    oauthActiveRef.current = false;
    oauthLoginIdRef.current = null;
    oauthCompletingRef.current = false;
    setOauthUrl(null);
    setOauthCallbackUrl(null);
    setOauthUrlCopied(false);
    setOauthUserCode(null);
    setOauthUserCodeCopied(false);
    setOauthMeta(null);
    setOauthPrepareError(null);
    setOauthCompleteError(null);
    setOauthTimedOut(false);
    setOauthPolling(false);
    setOauthManualCallbackInput('');
    setOauthManualCallbackSubmitting(false);
    setOauthManualCallbackError(null);
  }, [
    showAddModal,
    addTab,
    oauthLog,
    oauthService,
    oauthTabKeys,
    oauthUrl,
    oauthCallbackUrl,
    oauthUrlCopied,
    oauthUserCode,
    oauthUserCodeCopied,
    oauthMeta,
    oauthPrepareError,
    oauthCompleteError,
    oauthTimedOut,
    oauthPolling,
    oauthManualCallbackInput,
    oauthManualCallbackSubmitting,
    oauthManualCallbackError,
  ]);

  useEffect(
    () => () => {
      oauthAttemptSeqRef.current += 1;
      const loginId = oauthLoginIdRef.current ?? undefined;
      if (loginId) {
        oauthLog('页面卸载，准备取消授权流程', { loginId });
        oauthServiceRef.current?.cancelLogin(loginId).catch(() => {});
      }
      oauthActiveRef.current = false;
      oauthCompletingRef.current = false;
      oauthLoginIdRef.current = null;
    },
    [oauthLog],
  );

  const handleCopyOauthUrl = useCallback(async () => {
    if (!oauthUrl) return;
    try {
      await navigator.clipboard.writeText(oauthUrl);
      oauthLog('已复制授权链接', {
        loginId: oauthLoginIdRef.current,
        authUrl: oauthUrl,
      });
      setOauthUrlCopied(true);
      window.setTimeout(() => setOauthUrlCopied(false), 1200);
    } catch (e) {
      console.error('复制失败:', e);
    }
  }, [oauthUrl, oauthLog]);

  const handleCopyOauthUserCode = useCallback(async () => {
    if (!oauthUserCode) return;
    try {
      await navigator.clipboard.writeText(oauthUserCode);
      oauthLog('已复制 user_code', { loginId: oauthLoginIdRef.current });
      setOauthUserCodeCopied(true);
      window.setTimeout(() => setOauthUserCodeCopied(false), 1200);
    } catch (e) {
      console.error('复制失败:', e);
    }
  }, [oauthUserCode, oauthLog]);

  const handleRetryOauth = useCallback(() => {
    const previousLoginId = oauthLoginIdRef.current ?? undefined;
    oauthLog('用户点击刷新授权信息', {
      loginId: previousLoginId,
      error: oauthCompleteError,
      timedOut: oauthTimedOut,
    });
    oauthAttemptSeqRef.current += 1;
    if (previousLoginId) {
      oauthService?.cancelLogin(previousLoginId).catch(() => {});
    }
    oauthActiveRef.current = false;
    oauthLoginIdRef.current = null;
    oauthCompletingRef.current = false;
    setOauthPrepareError(null);
    setOauthCompleteError(null);
    setOauthTimedOut(false);
    setOauthPolling(false);
    setOauthMeta(null);
    setOauthUrl(null);
    setOauthCallbackUrl(null);
    setOauthUrlCopied(false);
    setOauthUserCode(null);
    setOauthUserCodeCopied(false);
    setOauthManualCallbackInput('');
    setOauthManualCallbackSubmitting(false);
    setOauthManualCallbackError(null);
    prepareOauthUrl();
  }, [oauthCompleteError, oauthTimedOut, oauthLog, oauthService, prepareOauthUrl]);

  const handleRetryOauthComplete = useCallback(() => {
    if (!oauthService) return;
    const loginId = oauthLoginIdRef.current;
    if (!loginId) return;
    if (oauthCompletingRef.current) return;
    const attemptSeq = ++oauthAttemptSeqRef.current;

    oauthLog('用户点击重新轮询授权结果', {
      loginId,
      error: oauthCompleteError,
      timedOut: oauthTimedOut,
      attemptSeq,
    });

    setOauthPrepareError(null);
    setOauthCompleteError(null);
    setOauthTimedOut(false);
    setOauthPolling(true);
    setOauthManualCallbackError(null);
    oauthCompletingRef.current = true;
    oauthActiveRef.current = false;

    oauthService
      .completeLogin(loginId)
      .then(async () => {
        if (attemptSeq !== oauthAttemptSeqRef.current) {
          oauthLog('忽略过期 OAuth 重试成功回调', { loginId, attemptSeq });
          return;
        }
        setOauthPolling(false);
        oauthCompletingRef.current = false;
        await completeOauthSuccess();
      })
      .catch((e) => {
        if (attemptSeq !== oauthAttemptSeqRef.current) {
          oauthLog('忽略过期 OAuth 重试异常回调', {
            loginId,
            attemptSeq,
            error: String(e),
          });
          return;
        }
        handleOauthCompleteError(e);
      });
  }, [
    oauthService,
    oauthLog,
    oauthCompleteError,
    oauthTimedOut,
    completeOauthSuccess,
    handleOauthCompleteError,
  ]);

  const handleOpenOauthUrl = useCallback(async () => {
    if (!oauthUrl) return;
    oauthLog('用户点击打开授权链接', {
      loginId: oauthLoginIdRef.current,
      authUrl: oauthUrl,
    });
    try {
      if (oauthService?.openAuthUrl) {
        await oauthService.openAuthUrl(oauthUrl);
      } else {
        await openUrl(oauthUrl);
      }
    } catch (e) {
      console.error('打开授权链接失败:', e);
      await navigator.clipboard.writeText(oauthUrl).catch(() => {});
      setOauthUrlCopied(true);
      setTimeout(() => setOauthUrlCopied(false), 1200);
    }
  }, [oauthUrl, oauthLog, oauthService]);

  const oauthSupportsManualCallback = useMemo(
    () => Boolean(oauthService?.submitCallbackUrl && oauthCallbackUrl),
    [oauthService, oauthCallbackUrl],
  );

  const handleSubmitOauthCallbackUrl = useCallback(async () => {
    if (!oauthService?.submitCallbackUrl) return;
    const loginId = oauthLoginIdRef.current;
    const callbackUrl = oauthManualCallbackInput.trim();
    if (!callbackUrl) return;
    if (!loginId) {
      setOauthManualCallbackError(t('common.shared.oauth.failed', '授权失败'));
      return;
    }

    setOauthManualCallbackSubmitting(true);
    setOauthManualCallbackError(null);
    try {
      await oauthService.submitCallbackUrl(loginId, callbackUrl);
      if (!oauthCompletingRef.current) {
        handleRetryOauthComplete();
      }
    } catch (e) {
      const msg = String(e).replace(/^Error:\s*/, '');
      setOauthManualCallbackError(msg);
    } finally {
      setOauthManualCallbackSubmitting(false);
    }
  }, [
    oauthService,
    oauthManualCallbackInput,
    t,
    handleRetryOauthComplete,
  ]);

  // ─── Flow Notice ──────────────────────────────────────────────────────
  const [isFlowNoticeCollapsed, setIsFlowNoticeCollapsed] = useState<boolean>(() => {
    if (!flowNoticeCollapsedKey) return false;
    try {
      return localStorage.getItem(flowNoticeCollapsedKey) === '1';
    } catch {
      return false;
    }
  });

  useEffect(() => {
    if (!flowNoticeCollapsedKey) return;
    try {
      localStorage.setItem(flowNoticeCollapsedKey, isFlowNoticeCollapsed ? '1' : '0');
    } catch {
      // ignore persistence failures
    }
  }, [isFlowNoticeCollapsed, flowNoticeCollapsedKey]);

  // ─── Current Account ──────────────────────────────────────────────────
  const [currentAccountId, setCurrentAccountId] = useState<string | null>(() => {
    if (!currentAccountIdKey) return null;
    try {
      const value = localStorage.getItem(currentAccountIdKey);
      return value && value.trim() ? value : null;
    } catch {
      return null;
    }
  });

  useEffect(() => {
    if (!currentAccountId) return;
    const exists = accounts.some((account) => account.id === currentAccountId);
    if (!exists) {
      setCurrentAccountId(null);
    }
  }, [accounts, currentAccountId]);

  useEffect(() => {
    if (!currentAccountIdKey) return;
    try {
      if (currentAccountId) {
        localStorage.setItem(currentAccountIdKey, currentAccountId);
      } else {
        localStorage.removeItem(currentAccountIdKey);
      }
    } catch {
      // ignore persistence failures
    }
  }, [currentAccountId, currentAccountIdKey]);

  // ─── Utilities ────────────────────────────────────────────────────────
  const formatDate = useCallback(
    (timestamp: number) => {
      const d = new Date(timestamp * 1000);
      return (
        d.toLocaleDateString(locale, {
          year: 'numeric',
          month: '2-digit',
          day: '2-digit',
        }) +
        ' ' +
        d.toLocaleTimeString(locale, { hour: '2-digit', minute: '2-digit' })
      );
    },
    [locale],
  );

  // ─── Return ───────────────────────────────────────────────────────────
  return {
    t,
    locale,
    privacyModeEnabled,
    togglePrivacyMode,
    maskAccountText,
    viewMode,
    setViewMode,
    searchQuery,
    setSearchQuery,
    filterType,
    setFilterType,
    sortBy,
    setSortBy,
    sortDirection,
    setSortDirection,
    selected,
    setSelected,
    toggleSelect,
    toggleSelectAll,
    tagFilter,
    setTagFilter,
    groupByTag,
    setGroupByTag,
    showTagFilter,
    setShowTagFilter,
    showTagModal,
    setShowTagModal,
    tagFilterRef,
    availableTags,
    toggleTagFilterValue,
    clearTagFilter,
    tagDeleteConfirm,
    setTagDeleteConfirm,
    deletingTag,
    requestDeleteTag,
    confirmDeleteTag,
    openTagModal,
    handleSaveTags,
    refreshing,
    refreshingAll,
    injecting,
    handleRefresh,
    handleRefreshAll,
    handleDelete,
    handleBatchDelete,
    deleteConfirm,
    setDeleteConfirm,
    deleting,
    confirmDelete,
    message,
    setMessage,
    exporting,
    handleExport,
    handleExportByIds,
    showExportModal: exportModal.showModal,
    closeExportModal: exportModal.closeModal,
    exportJsonContent: exportModal.jsonContent,
    exportJsonHidden: exportModal.hidden,
    toggleExportJsonHidden: exportModal.toggleHidden,
    exportJsonCopied: exportModal.copied,
    copyExportJson: exportModal.copyJson,
    savingExportJson: exportModal.saving,
    saveExportJson: exportModal.saveJson,
    exportSavedPath: exportModal.savedPath,
    canOpenExportSavedDirectory: exportModal.canOpenSavedDirectory,
    openExportSavedDirectory: exportModal.openSavedDirectory,
    copyExportSavedPath: exportModal.copySavedPath,
    exportPathCopied: exportModal.pathCopied,
    showAddModal,
    setShowAddModal,
    addTab,
    setAddTab,
    addStatus,
    setAddStatus,
    addMessage,
    setAddMessage,
    tokenInput,
    setTokenInput,
    importing,
    openAddModal,
    closeAddModal,
    resetAddModalState,
    handleTokenImport,
    handleImportJsonFile,
    handleImportFromLocal,
    handlePickImportFile,
    importFileInputRef,
    oauthUrl,
    oauthCallbackUrl,
    oauthUrlCopied,
    oauthUserCode,
    oauthUserCodeCopied,
    oauthMeta,
    oauthPrepareError,
    oauthCompleteError,
    oauthPolling,
    oauthTimedOut,
    oauthManualCallbackInput,
    setOauthManualCallbackInput,
    oauthManualCallbackSubmitting,
    oauthManualCallbackError,
    oauthSupportsManualCallback,
    handleCopyOauthUrl,
    handleCopyOauthUserCode,
    handleRetryOauth,
    handleRetryOauthComplete,
    handleOpenOauthUrl,
    handleSubmitOauthCallbackUrl,
    handleInjectToVSCode,
    isFlowNoticeCollapsed,
    setIsFlowNoticeCollapsed,
    currentAccountId,
    setCurrentAccountId,
    formatDate,
    normalizeTag,
    resolveDefaultExportPath: exportModal.resolveDefaultExportPath,
    saveJsonFile: exportModal.saveJsonFile,
  };
}
