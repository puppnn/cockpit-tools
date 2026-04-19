import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
} from 'react';
import { useTranslation } from 'react-i18next';
import { confirm as confirmDialog } from '@tauri-apps/plugin-dialog';
import {
  AlertTriangle,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  Eye,
  Folder,
  GripVertical,
  RefreshCw,
  RotateCcw,
  Save,
  Search,
  Star,
  Trash2,
  X,
} from 'lucide-react';
import { ModalErrorMessage, useModalErrorState } from '../ModalErrorMessage';
import type {
  CodexSessionFavoriteResult,
  CodexSessionTitleUpdateResult,
  CodexSessionTokenStats,
  CodexSessionViewerRecord,
  CodexTimelineEvent,
  CodexTrashedSessionRecord,
} from '../../types/codex';
import { useCodexInstanceStore } from '../../stores/useCodexInstanceStore';

type MessageState = { text: string; tone?: 'error' };
type SessionTokenStatsMap = Record<string, CodexSessionTokenStats>;
type ResizeSide = 'left' | 'right';
type ResizeState = {
  side: ResizeSide;
  startX: number;
  startLeft: number;
  startRight: number;
  containerWidth: number;
};

type SessionGroup = {
  cwd: string;
  sessions: CodexSessionViewerRecord[];
  latestUpdatedAt: number;
};

const ASSISTANT_PREVIEW_LIMIT = 200;
const USER_PREVIEW_LIMIT = 260;
const MIN_LEFT_PANEL = 280;
const MAX_LEFT_PANEL = 460;
const MIN_RIGHT_PANEL = 320;
const MAX_RIGHT_PANEL = 520;
const MIN_CENTER_PANEL = 420;
const RESIZE_GAP_ALLOWANCE = 40;
const LEADING_ENVIRONMENT_CONTEXT_REGEX =
  /^\s*<environment_context>[\s\S]*?<\/environment_context>\s*/u;

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function formatRelativeTime(
  value: number | null | undefined,
  t: ReturnType<typeof useTranslation>['t'],
): string {
  if (!value) return t('codex.sessionManager.viewer.timeUnknown', '时间未知');

  const diffSeconds = Math.max(0, Math.floor(Date.now() / 1000) - value);
  const minute = 60;
  const hour = 60 * minute;
  const day = 24 * hour;

  if (diffSeconds < hour) {
    return t('codex.sessionManager.viewer.minutesAgo', '{{count}} 分钟前', {
      count: Math.max(1, Math.floor(diffSeconds / minute)),
    });
  }
  if (diffSeconds < day) {
    return t('codex.sessionManager.viewer.hoursAgo', '{{count}} 小时前', {
      count: Math.max(1, Math.floor(diffSeconds / hour)),
    });
  }
  return t('codex.sessionManager.viewer.daysAgo', '{{count}} 天前', {
    count: Math.max(1, Math.floor(diffSeconds / day)),
  });
}

function formatAbsoluteTime(value: number | null | undefined): string {
  if (!value) return '-';
  const date = new Date(value * 1000);
  return Number.isNaN(date.getTime())
    ? '-'
    : date.toLocaleString('zh-CN', { hour12: false });
}

function normalizeBody(value: string): string {
  return value.replace(/\s+/g, ' ').trim();
}

function getDisplayBody(event: CodexTimelineEvent): string {
  if (event.kind !== 'user_message') {
    return event.body;
  }

  return event.body.replace(LEADING_ENVIRONMENT_CONTEXT_REGEX, '').trim();
}

function buildConversationEvents(events: CodexTimelineEvent[]): CodexTimelineEvent[] {
  const visible: CodexTimelineEvent[] = [];
  let previousKey = '';

  for (const event of events) {
    const normalizedBody = normalizeBody(getDisplayBody(event));
    if (
      !normalizedBody ||
      (event.kind !== 'user_message' && event.kind !== 'assistant_message')
    ) {
      continue;
    }

    const key = `${event.kind}:${normalizedBody}`;
    if (key === previousKey) continue;

    visible.push(event);
    previousKey = key;
  }

  return visible;
}

function isLongMessage(event: CodexTimelineEvent): boolean {
  const body = getDisplayBody(event);
  const normalized = normalizeBody(body);
  return event.kind === 'assistant_message'
    ? normalized.length > ASSISTANT_PREVIEW_LIMIT || body.split(/\r?\n/u).length > 5
    : normalized.length > USER_PREVIEW_LIMIT || body.split(/\r?\n/u).length > 7;
}

function previewMessage(event: CodexTimelineEvent): string {
  const normalized = normalizeBody(getDisplayBody(event));
  const limit =
    event.kind === 'assistant_message'
      ? ASSISTANT_PREVIEW_LIMIT
      : USER_PREVIEW_LIMIT;
  return normalized.length > limit
    ? `${normalized.slice(0, limit)}...`
    : normalized;
}

function matchesSessionQuery(session: CodexSessionViewerRecord, query: string): boolean {
  const text = [
    session.sessionId,
    session.title,
    session.cwd,
    session.modelProvider,
    ...session.locations.map(
      (location) =>
        `${location.instanceName} ${location.cwd} ${location.modelProvider}`,
    ),
  ]
    .join('\n')
    .toLowerCase();
  return text.includes(query);
}

function formatTitleSaveMessage(
  result: CodexSessionTitleUpdateResult,
  t: ReturnType<typeof useTranslation>['t'],
): string {
  if (result.warnings.length === 0) {
    return t('codex.sessionManager.viewer.titleSaved', '标题已保存，已更新 {{count}} 个实例。', {
      count: result.matchedInstanceCount,
    });
  }
  return `${t('codex.sessionManager.viewer.titleSavedWithWarnings', '标题已保存，但有警告：')} ${result.warnings.join(' | ')}`;
}

function formatFavoriteMessage(
  result: CodexSessionFavoriteResult,
  t: ReturnType<typeof useTranslation>['t'],
): string {
  if (result.warnings.length === 0) {
    return t('codex.sessionManager.viewer.favoriteSaved', '会话已收藏并完成备份。');
  }
  return `${t('codex.sessionManager.viewer.favoriteSavedWithWarnings', '会话已收藏，但有警告：')} ${result.warnings.join(' | ')}`;
}

function formatUnfavoriteMessage(
  result: CodexSessionFavoriteResult,
  t: ReturnType<typeof useTranslation>['t'],
): string {
  if (result.warnings.length === 0) {
    return t('codex.sessionManager.viewer.favoriteRemoved', '已取消收藏，并删除备份。');
  }
  return `${t('codex.sessionManager.viewer.favoriteRemovedWithWarnings', '已取消收藏，但有警告：')} ${result.warnings.join(' | ')}`;
}

function buildSessionGroups(sessions: CodexSessionViewerRecord[]): SessionGroup[] {
  const groups = new Map<string, CodexSessionViewerRecord[]>();

  sessions.forEach((session) => {
    const key = session.cwd || '__no_cwd__';
    const bucket = groups.get(key) ?? [];
    bucket.push(session);
    groups.set(key, bucket);
  });

  return Array.from(groups.entries())
    .map(([cwd, groupSessions]) => ({
      cwd,
      sessions: [...groupSessions].sort(
        (left, right) =>
          (right.updatedAt ?? 0) - (left.updatedAt ?? 0) ||
          left.title.localeCompare(right.title, 'zh-CN'),
      ),
      latestUpdatedAt: Math.max(...groupSessions.map((item) => item.updatedAt ?? 0), 0),
    }))
    .sort(
      (left, right) =>
        right.latestUpdatedAt - left.latestUpdatedAt ||
        left.cwd.localeCompare(right.cwd, 'zh-CN'),
    );
}

function resolveGroupLabel(cwd: string, t: ReturnType<typeof useTranslation>['t']): string {
  if (!cwd || cwd === '__no_cwd__') {
    return t('codex.sessionManager.viewer.noCwd', '未记录工作目录');
  }

  const normalized = cwd.replace(/\\/g, '/').replace(/\/$/, '');
  const parts = normalized.split('/').filter(Boolean);
  return parts[parts.length - 1] || cwd;
}

function formatSessionId(sessionId: string): string {
  if (sessionId.length <= 18) return sessionId;
  return `${sessionId.slice(0, 8)}...${sessionId.slice(-6)}`;
}

function formatLargeNumber(value: number): string {
  if (value >= 1_000_000) {
    return `${(value / 1_000_000).toFixed(1)}M`;
  }
  if (value >= 1_000) {
    return `${(value / 1_000).toFixed(1)}K`;
  }
  return value.toLocaleString();
}

function formatTokenStats(stats?: CodexSessionTokenStats): string {
  if (!stats) return '';
  return `${formatLargeNumber(stats.inputTokens)} / ${formatLargeNumber(stats.outputTokens)} tokens`;
}

export function CodexSessionManager() {
  const { t } = useTranslation();
  const instances = useCodexInstanceStore((state) => state.instances);
  const refreshInstances = useCodexInstanceStore((state) => state.refreshInstances);
  const syncThreadsAcrossInstances = useCodexInstanceStore(
    (state) => state.syncThreadsAcrossInstances,
  );
  const repairSessionVisibilityAcrossInstances = useCodexInstanceStore(
    (state) => state.repairSessionVisibilityAcrossInstances,
  );
  const listSessionsForViewer = useCodexInstanceStore(
    (state) => state.listSessionsForViewer,
  );
  const getSessionTokenStatsAcrossInstances = useCodexInstanceStore(
    (state) => state.getSessionTokenStatsAcrossInstances,
  );
  const getSessionTimeline = useCodexInstanceStore(
    (state) => state.getSessionTimeline,
  );
  const updateSessionTitle = useCodexInstanceStore(
    (state) => state.updateSessionTitle,
  );
  const favoriteSession = useCodexInstanceStore(
    (state) => state.favoriteSession,
  );
  const unfavoriteSession = useCodexInstanceStore(
    (state) => state.unfavoriteSession,
  );
  const moveSessionsToTrashAcrossInstances = useCodexInstanceStore(
    (state) => state.moveSessionsToTrashAcrossInstances,
  );
  const listTrashedSessionsAcrossInstances = useCodexInstanceStore(
    (state) => state.listTrashedSessionsAcrossInstances,
  );
  const restoreSessionsFromTrashAcrossInstances = useCodexInstanceStore(
    (state) => state.restoreSessionsFromTrashAcrossInstances,
  );

  const [sessions, setSessions] = useState<CodexSessionViewerRecord[]>([]);
  const [trashedSessions, setTrashedSessions] = useState<CodexTrashedSessionRecord[]>([]);
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [expandedGroups, setExpandedGroups] = useState<string[]>([]);
  const [selectedTrashIds, setSelectedTrashIds] = useState<string[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState('');
  const [selectedLocationInstanceId, setSelectedLocationInstanceId] = useState<string | null>(
    null,
  );
  const [timeline, setTimeline] = useState<{ events: CodexTimelineEvent[]; warnings: string[] }>(
    { events: [], warnings: [] },
  );
  const [selectedEventId, setSelectedEventId] = useState('');
  const [expandedEventIds, setExpandedEventIds] = useState<string[]>([]);
  const [query, setQuery] = useState('');
  const [titleDraft, setTitleDraft] = useState('');
  const [loading, setLoading] = useState(false);
  const [loadingTimeline, setLoadingTimeline] = useState(false);
  const [loadingTrash, setLoadingTrash] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [repairingVisibility, setRepairingVisibility] = useState(false);
  const [savingTitle, setSavingTitle] = useState(false);
  const [favoriting, setFavoriting] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [restoring, setRestoring] = useState(false);
  const [showRestoreModal, setShowRestoreModal] = useState(false);
  const [message, setMessage] = useState<MessageState | null>(null);
  const [copiedSessionId, setCopiedSessionId] = useState<string | null>(null);
  const [tokenStatsBySessionId, setTokenStatsBySessionId] = useState<SessionTokenStatsMap>({});
  const [loadingTokenGroupCwds, setLoadingTokenGroupCwds] = useState<string[]>([]);
  const [loadedTokenGroupCwds, setLoadedTokenGroupCwds] = useState<string[]>([]);
  const [panelWidths, setPanelWidths] = useState({ left: 320, right: 360 });
  const [activeResizer, setActiveResizer] = useState<ResizeSide | null>(null);

  const viewerRef = useRef<HTMLDivElement | null>(null);
  const loadSessionsPromiseRef = useRef<Promise<void> | null>(null);
  const timelineRequestIdRef = useRef(0);
  const resizeStateRef = useRef<ResizeState | null>(null);
  const copyResetTimerRef = useRef<number | null>(null);
  const tokenStatsVersionRef = useRef(0);
  const {
    message: restoreModalError,
    scrollKey: restoreModalErrorScrollKey,
    set: setRestoreModalError,
  } = useModalErrorState();

  const selectedSession = useMemo(
    () => sessions.find((item) => item.sessionId === selectedSessionId) ?? null,
    [selectedSessionId, sessions],
  );
  const selectedEvent = useMemo(
    () => timeline.events.find((item) => item.id === selectedEventId) ?? null,
    [selectedEventId, timeline.events],
  );
  const conversationEvents = useMemo(
    () => buildConversationEvents(timeline.events),
    [timeline.events],
  );
  const filteredSessions = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    if (!normalizedQuery) return sessions;
    return sessions.filter((session) => matchesSessionQuery(session, normalizedQuery));
  }, [query, sessions]);
  const groupedSessions = useMemo(
    () => buildSessionGroups(filteredSessions),
    [filteredSessions],
  );
  const selectedIdSet = useMemo(() => new Set(selectedIds), [selectedIds]);
  const selectedTrashIdSet = useMemo(() => new Set(selectedTrashIds), [selectedTrashIds]);
  const loadingTokenGroupSet = useMemo(() => new Set(loadingTokenGroupCwds), [loadingTokenGroupCwds]);
  const loadedTokenGroupSet = useMemo(() => new Set(loadedTokenGroupCwds), [loadedTokenGroupCwds]);
  const selectedLocation = useMemo(
    () =>
      selectedSession?.locations.find(
        (location) => location.instanceId === selectedLocationInstanceId,
      ) ?? selectedSession?.locations[0] ?? null,
    [selectedLocationInstanceId, selectedSession],
  );
  const viewerStyle = useMemo(
    () =>
      ({
        '--codex-session-left': `${panelWidths.left}px`,
        '--codex-session-right': `${panelWidths.right}px`,
      }) as CSSProperties,
    [panelWidths],
  );

  const loadSessions = useCallback(async () => {
    if (loadSessionsPromiseRef.current) return await loadSessionsPromiseRef.current;

    const task = (async () => {
      setLoading(true);
      try {
        const nextSessions = await listSessionsForViewer();
        tokenStatsVersionRef.current += 1;
        setSessions(nextSessions);
        setTokenStatsBySessionId({});
        setLoadingTokenGroupCwds([]);
        setLoadedTokenGroupCwds([]);
        setSelectedIds((prev) =>
          prev.filter((id) => nextSessions.some((item) => item.sessionId === id)),
        );
      } catch (error) {
        setMessage({ text: String(error), tone: 'error' });
      } finally {
        setLoading(false);
      }
    })();

    loadSessionsPromiseRef.current = task;

    try {
      await task;
    } finally {
      if (loadSessionsPromiseRef.current === task) {
        loadSessionsPromiseRef.current = null;
      }
    }
  }, [listSessionsForViewer]);

  const loadTokenStatsForGroups = useCallback(
    async (groups: SessionGroup[]) => {
      if (groups.length === 0) return;

      const groupCwds = groups.map((group) => group.cwd);
      const sessionIds = Array.from(
        new Set(groups.flatMap((group) => group.sessions.map((session) => session.sessionId))),
      );
      if (sessionIds.length === 0) {
        setLoadedTokenGroupCwds((prev) => Array.from(new Set([...prev, ...groupCwds])));
        return;
      }

      const requestVersion = tokenStatsVersionRef.current;
      setLoadingTokenGroupCwds((prev) => Array.from(new Set([...prev, ...groupCwds])));

      try {
        const stats = await getSessionTokenStatsAcrossInstances(sessionIds);
        if (tokenStatsVersionRef.current !== requestVersion) return;

        setTokenStatsBySessionId((prev) => {
          const next = { ...prev };
          stats.forEach((item) => {
            next[item.sessionId] = item;
          });
          return next;
        });
      } catch (error) {
        if (tokenStatsVersionRef.current === requestVersion) {
          console.error('Failed to load session token stats:', error);
        }
      } finally {
        if (tokenStatsVersionRef.current !== requestVersion) return;
        setLoadingTokenGroupCwds((prev) => prev.filter((cwd) => !groupCwds.includes(cwd)));
        setLoadedTokenGroupCwds((prev) => Array.from(new Set([...prev, ...groupCwds])));
      }
    },
    [getSessionTokenStatsAcrossInstances],
  );

  const loadTimeline = useCallback(
    async (sessionId: string, instanceId?: string | null) => {
      const requestId = timelineRequestIdRef.current + 1;
      timelineRequestIdRef.current = requestId;
      setLoadingTimeline(true);

      try {
        const result = await getSessionTimeline(sessionId, instanceId ?? null);
        if (timelineRequestIdRef.current !== requestId) return;

        const nextConversationEvents = buildConversationEvents(result.events);
        setTimeline({ events: result.events, warnings: result.warnings });
        setSelectedEventId(nextConversationEvents[0]?.id ?? result.events[0]?.id ?? '');
        setExpandedEventIds([]);
      } catch (error) {
        if (timelineRequestIdRef.current !== requestId) return;
        setTimeline({ events: [], warnings: [String(error)] });
        setSelectedEventId('');
      } finally {
        if (timelineRequestIdRef.current === requestId) {
          setLoadingTimeline(false);
        }
      }
    },
    [getSessionTimeline],
  );

  const loadTrashedSessions = useCallback(async () => {
    setLoadingTrash(true);
    setRestoreModalError(null);

    try {
      const nextSessions = await listTrashedSessionsAcrossInstances();
      setTrashedSessions(nextSessions);
      setSelectedTrashIds((prev) =>
        prev.filter((id) => nextSessions.some((item) => item.sessionId === id)),
      );
      return nextSessions;
    } catch (error) {
      setRestoreModalError(String(error));
      return [];
    } finally {
      setLoadingTrash(false);
    }
  }, [listTrashedSessionsAcrossInstances, setRestoreModalError]);

  const closeRestoreModal = useCallback(() => {
    if (restoring) return;
    setShowRestoreModal(false);
    setSelectedTrashIds([]);
    setRestoreModalError(null);
  }, [restoring, setRestoreModalError]);

  const beginResize = useCallback(
    (side: ResizeSide) => (event: ReactPointerEvent<HTMLDivElement>) => {
      if (window.innerWidth <= 1180) return;

      const rect = viewerRef.current?.getBoundingClientRect();
      if (!rect) return;

      resizeStateRef.current = {
        side,
        startX: event.clientX,
        startLeft: panelWidths.left,
        startRight: panelWidths.right,
        containerWidth: rect.width,
      };
      setActiveResizer(side);

      if (event.currentTarget.setPointerCapture) {
        event.currentTarget.setPointerCapture(event.pointerId);
      }
      event.preventDefault();
    },
    [panelWidths.left, panelWidths.right],
  );

  useEffect(() => {
    const handlePointerMove = (event: PointerEvent) => {
      const state = resizeStateRef.current;
      if (!state) return;

      const delta = event.clientX - state.startX;
      if (state.side === 'left') {
        const maxLeft = Math.min(
          MAX_LEFT_PANEL,
          state.containerWidth - state.startRight - MIN_CENTER_PANEL - RESIZE_GAP_ALLOWANCE,
        );
        setPanelWidths((prev) => ({
          ...prev,
          left: clamp(state.startLeft + delta, MIN_LEFT_PANEL, Math.max(MIN_LEFT_PANEL, maxLeft)),
        }));
        return;
      }

      const maxRight = Math.min(
        MAX_RIGHT_PANEL,
        state.containerWidth - state.startLeft - MIN_CENTER_PANEL - RESIZE_GAP_ALLOWANCE,
      );
      setPanelWidths((prev) => ({
        ...prev,
        right: clamp(state.startRight - delta, MIN_RIGHT_PANEL, Math.max(MIN_RIGHT_PANEL, maxRight)),
      }));
    };

    const stopResize = () => {
      resizeStateRef.current = null;
      setActiveResizer(null);
    };

    window.addEventListener('pointermove', handlePointerMove);
    window.addEventListener('pointerup', stopResize);
    window.addEventListener('pointercancel', stopResize);

    return () => {
      window.removeEventListener('pointermove', handlePointerMove);
      window.removeEventListener('pointerup', stopResize);
      window.removeEventListener('pointercancel', stopResize);
    };
  }, []);

  useEffect(() => {
    void loadSessions();
  }, [loadSessions]);

  useEffect(() => {
    if (!filteredSessions.length) {
      setSelectedSessionId('');
      return;
    }
    if (!filteredSessions.some((item) => item.sessionId === selectedSessionId)) {
      setSelectedSessionId(filteredSessions[0].sessionId);
    }
  }, [filteredSessions, selectedSessionId]);

  useEffect(() => {
    if (!selectedSession) {
      setSelectedLocationInstanceId(null);
      return;
    }
    setSelectedLocationInstanceId((prev) =>
      prev && selectedSession.locations.some((location) => location.instanceId === prev)
        ? prev
        : selectedSession.locations[0]?.instanceId ?? null,
    );
  }, [selectedSession]);

  useEffect(() => {
    if (!selectedSession) {
      setTitleDraft('');
      return;
    }
    setTitleDraft(selectedSession.title);
  }, [selectedSession]);

  useEffect(() => {
    if (selectedSession) {
      void loadTimeline(selectedSession.sessionId, selectedLocationInstanceId);
    }
  }, [loadTimeline, selectedLocationInstanceId, selectedSession]);

  useEffect(() => {
    if (showRestoreModal) {
      void loadTrashedSessions();
    }
  }, [loadTrashedSessions, showRestoreModal]);

  useEffect(() => {
    setExpandedGroups((prev) => {
      const valid = prev.filter((cwd) => groupedSessions.some((group) => group.cwd === cwd));
      if (query.trim()) {
        return groupedSessions.map((group) => group.cwd);
      }
      return valid;
    });
  }, [groupedSessions, query]);

  useEffect(() => {
    const groupsToLoad = groupedSessions.filter(
      (group) =>
        expandedGroups.includes(group.cwd) &&
        !loadedTokenGroupSet.has(group.cwd) &&
        !loadingTokenGroupSet.has(group.cwd),
    );
    if (groupsToLoad.length > 0) {
      void loadTokenStatsForGroups(groupsToLoad);
    }
  }, [
    expandedGroups,
    groupedSessions,
    loadedTokenGroupSet,
    loadingTokenGroupSet,
    loadTokenStatsForGroups,
  ]);

  useEffect(
    () => () => {
      if (copyResetTimerRef.current !== null) {
        window.clearTimeout(copyResetTimerRef.current);
      }
    },
    [],
  );

  const toggleSession = (sessionId: string) => {
    setSelectedIds((prev) =>
      prev.includes(sessionId) ? prev.filter((id) => id !== sessionId) : [...prev, sessionId],
    );
  };

  const toggleGroupSelection = (sessionIds: string[], allSelected: boolean) => {
    setSelectedIds((prev) => {
      const next = new Set(prev);
      if (allSelected) {
        sessionIds.forEach((id) => next.delete(id));
      } else {
        sessionIds.forEach((id) => next.add(id));
      }
      return Array.from(next);
    });
  };

  const toggleGroupExpanded = (cwd: string) => {
    setExpandedGroups((prev) =>
      prev.includes(cwd) ? prev.filter((item) => item !== cwd) : [...prev, cwd],
    );
  };

  const handleSessionRowKeyDown = (
    event: ReactKeyboardEvent<HTMLDivElement>,
    sessionId: string,
  ) => {
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    setSelectedSessionId(sessionId);
  };

  const handleCopySessionId = async (
    event: ReactMouseEvent<HTMLButtonElement>,
    sessionId: string,
  ) => {
    event.stopPropagation();
    try {
      await navigator.clipboard.writeText(sessionId);
      setCopiedSessionId(sessionId);
      if (copyResetTimerRef.current !== null) {
        window.clearTimeout(copyResetTimerRef.current);
      }
      copyResetTimerRef.current = window.setTimeout(() => {
        setCopiedSessionId(null);
        copyResetTimerRef.current = null;
      }, 1400);
    } catch (error) {
      setMessage({
        text: t('common.shared.export.copyFailed', '复制失败：{{error}}', {
          error: String(error),
        }),
        tone: 'error',
      });
    }
  };

  const handleRefresh = async () => {
    setMessage(null);
    try {
      await refreshInstances();
      await loadSessions();
      if (showRestoreModal) {
        await loadTrashedSessions();
      }
    } catch (error) {
      setMessage({ text: String(error), tone: 'error' });
    }
  };

  const handleSyncSessions = async () => {
    setMessage(null);

    try {
      const latestInstances = await refreshInstances();
      if (latestInstances.length < 2) {
        setMessage({
          text: t('codex.sessionManager.viewer.syncNeedTwo', '至少需要两个实例才能同步会话'),
          tone: 'error',
        });
        return;
      }

      const confirmed = await confirmDialog(
        t(
          'codex.sessionManager.viewer.syncConfirm',
          '会将缺失的线程和对应会话同步到所有实例中，并在写入前备份目标文件。确认继续吗？',
        ),
        {
          title: t('codex.sessionManager.actions.syncSessions', '同步会话'),
          okLabel: t('common.confirm', '确认'),
          cancelLabel: t('common.cancel', '取消'),
        },
      );
      if (!confirmed) return;

      setSyncing(true);
      const summary = await syncThreadsAcrossInstances();
      setMessage({ text: summary.message });
      await loadSessions();
    } catch (error) {
      setMessage({ text: String(error), tone: 'error' });
    } finally {
      setSyncing(false);
    }
  };

  const handleRepairVisibility = async () => {
    const confirmed = await confirmDialog(
      t(
        'codex.sessionManager.viewer.repairConfirm',
        '会按各实例配置修复 provider 可见性，并在写入前备份相关文件。确认继续吗？',
      ),
      {
        title: t('codex.sessionManager.actions.repairVisibility', '修复可见性'),
        okLabel: t('common.confirm', '确认'),
        cancelLabel: t('common.cancel', '取消'),
      },
    );
    if (!confirmed) return;

    setRepairingVisibility(true);
    try {
      const summary = await repairSessionVisibilityAcrossInstances();
      setMessage({ text: summary.message });
      await loadSessions();
    } catch (error) {
      setMessage({ text: String(error), tone: 'error' });
    } finally {
      setRepairingVisibility(false);
    }
  };

  const handleMoveToTrash = async () => {
    if (selectedIds.length === 0) {
      setMessage({
        text: t('codex.sessionManager.viewer.pickOne', '请至少选择一条会话'),
        tone: 'error',
      });
      return;
    }

    const confirmed = await confirmDialog(
      t(
        'codex.sessionManager.confirm.message',
        '会将所选会话移到回收区，运行中的实例可能需要重启后才会反映。确认继续吗？',
      ),
      {
        title: t('codex.sessionManager.confirm.title', '移到回收区'),
        okLabel: t('common.confirm', '确认'),
        cancelLabel: t('common.cancel', '取消'),
        kind: 'warning',
      },
    );
    if (!confirmed) return;

    setDeleting(true);
    try {
      const summary = await moveSessionsToTrashAcrossInstances(selectedIds);
      setMessage({ text: summary.message });
      setSelectedIds([]);
      await loadSessions();
      if (showRestoreModal) {
        await loadTrashedSessions();
      }
    } catch (error) {
      setMessage({ text: String(error), tone: 'error' });
    } finally {
      setDeleting(false);
    }
  };

  const handleSaveTitle = async () => {
    if (!selectedSession) return;
    if (!titleDraft.trim()) {
      setMessage({
        text: t('codex.sessionManager.viewer.titleRequired', '标题不能为空'),
        tone: 'error',
      });
      return;
    }

    setSavingTitle(true);
    try {
      const result = await updateSessionTitle(selectedSession.sessionId, titleDraft.trim());
      await loadSessions();
      setMessage({
        text: formatTitleSaveMessage(result, t),
        tone: result.warnings.length > 0 ? 'error' : undefined,
      });
    } catch (error) {
      setMessage({ text: String(error), tone: 'error' });
    } finally {
      setSavingTitle(false);
    }
  };

  const handleFavoriteSession = async () => {
    if (!selectedSession) return;

    setFavoriting(true);
    try {
      const result = selectedSession.isFavorite
        ? await unfavoriteSession(selectedSession.sessionId)
        : await favoriteSession(selectedSession.sessionId);
      await loadSessions();
      setMessage({
        text: selectedSession.isFavorite
          ? formatUnfavoriteMessage(result, t)
          : formatFavoriteMessage(result, t),
        tone: result.warnings.length > 0 ? 'error' : undefined,
      });
    } catch (error) {
      setMessage({ text: String(error), tone: 'error' });
    } finally {
      setFavoriting(false);
    }
  };

  const handleRestoreFromTrash = async () => {
    if (selectedTrashIds.length === 0) {
      setRestoreModalError(
        t('codex.sessionManager.viewer.pickRestoreOne', '请至少选择一条待恢复会话'),
      );
      return;
    }

    setRestoring(true);
    try {
      const summary = await restoreSessionsFromTrashAcrossInstances(selectedTrashIds);
      setMessage({ text: summary.message });
      setSelectedTrashIds([]);
      const [nextTrashed] = await Promise.all([loadTrashedSessions(), loadSessions()]);
      if (nextTrashed.length === 0) {
        setShowRestoreModal(false);
      }
    } catch (error) {
      setRestoreModalError(String(error));
    } finally {
      setRestoring(false);
    }
  };

  return (
    <section className="codex-session-manager">
      <div className="codex-session-manager__toolbar">
        <div className="codex-session-manager__search">
          <Search size={14} />
          <input
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t('codex.sessionManager.viewer.searchPlaceholder', '搜索标题、路径、实例')}
          />
        </div>

        <div className="codex-session-manager__toolbar-actions">
          <button
            className="btn btn-secondary codex-session-manager__action-button"
            type="button"
            onClick={() => void handleSyncSessions()}
            disabled={
              syncing ||
              repairingVisibility ||
              deleting ||
              loading ||
              instances.length < 2
            }
          >
            <RefreshCw size={14} className={syncing ? 'icon-spin' : undefined} />
            {t('codex.sessionManager.actions.syncSessions', '同步会话')}
          </button>
          <button
            className="btn btn-secondary codex-session-manager__action-button"
            type="button"
            onClick={() => void handleRepairVisibility()}
            disabled={repairingVisibility || syncing || deleting || loading}
          >
            <Eye size={14} />
            {t('codex.sessionManager.actions.repairVisibility', '修复可见性')}
          </button>
          <button
            className="btn btn-secondary codex-session-manager__action-button"
            type="button"
            onClick={() => void setShowRestoreModal(true)}
            disabled={loading || syncing || repairingVisibility || deleting || restoring}
          >
            <RotateCcw size={14} />
            {t('codex.sessionManager.actions.restoreSessions', '恢复会话')}
          </button>
          <button
            className="btn btn-secondary codex-session-manager__action-button"
            type="button"
            onClick={() => void handleRefresh()}
            disabled={loading || deleting || syncing || repairingVisibility}
          >
            <RefreshCw size={14} className={loading ? 'icon-spin' : undefined} />
            {t('common.refresh', '刷新')}
          </button>
          <button
            className="btn btn-danger codex-session-manager__action-button"
            type="button"
            onClick={() => void handleMoveToTrash()}
            disabled={
              deleting ||
              loading ||
              syncing ||
              repairingVisibility ||
              selectedIds.length === 0
            }
          >
            <Trash2 size={14} />
            {t('codex.sessionManager.actions.moveToTrash', '移到回收区')} ({selectedIds.length})
          </button>
        </div>
      </div>

      {message ? (
        <div className={`message-bar ${message.tone === 'error' ? 'error' : 'success'}`}>
          {message.text}
        </div>
      ) : null}

      <div className="codex-session-viewer" ref={viewerRef} style={viewerStyle}>
        <aside className="codex-session-viewer__sidebar">
          <div className="codex-session-viewer__sidebar-head">
            {t('codex.sessionManager.viewer.visibleSessions', '共 {{count}} 条会话', {
              count: filteredSessions.length,
            })}
          </div>

          {!loading && filteredSessions.length === 0 ? (
            <div className="codex-session-viewer__empty">
              <Folder size={20} />
              <span>{t('codex.sessionManager.empty.title', '还没有可管理的会话')}</span>
            </div>
          ) : null}

          {groupedSessions.length > 0 ? (
            <div className="codex-session-manager__list">
              {groupedSessions.map((group) => {
                const groupSessionIds = group.sessions.map((item) => item.sessionId);
                const allSelected =
                  groupSessionIds.length > 0 &&
                  groupSessionIds.every((id) => selectedIdSet.has(id));
                const isExpanded = expandedGroups.includes(group.cwd);

                return (
                  <section className="codex-session-folder" key={group.cwd}>
                    <div className="codex-session-folder__row">
                      <div className="codex-session-folder__left">
                        <button
                          className="codex-session-folder__expand"
                          type="button"
                          onClick={() => toggleGroupExpanded(group.cwd)}
                          aria-label={
                            isExpanded
                              ? t('codex.sessionManager.actions.collapse', '收起')
                              : t('codex.sessionManager.actions.expand', '展开')
                          }
                        >
                          {isExpanded ? <ChevronDown size={16} /> : <ChevronRight size={16} />}
                        </button>
                        <input
                          className="codex-session-folder__checkbox"
                          type="checkbox"
                          checked={allSelected}
                          onChange={() => toggleGroupSelection(groupSessionIds, allSelected)}
                        />
                        <Folder size={16} className="codex-session-folder__icon" />
                        <button
                          className="codex-session-folder__label"
                          type="button"
                          onClick={() => toggleGroupExpanded(group.cwd)}
                          title={group.cwd}
                        >
                          {resolveGroupLabel(group.cwd, t)}
                        </button>
                      </div>
                      <span className="codex-session-folder__time">
                        {formatRelativeTime(group.latestUpdatedAt, t)}
                      </span>
                    </div>

                    {isExpanded ? (
                      <div className="codex-session-folder__children">
                        {group.sessions.map((session) => {
                          const hasRunningLocation = session.locations.some(
                            (location) => location.running,
                          );
                          const tokenText = formatTokenStats(
                            tokenStatsBySessionId[session.sessionId],
                          );
                          const isTokenStatsLoading = loadingTokenGroupSet.has(group.cwd);

                          return (
                            <div
                              className={`codex-session-row${
                                selectedSessionId === session.sessionId ? ' active' : ''
                              }`}
                              key={session.sessionId}
                              role="button"
                              tabIndex={0}
                              onClick={() => setSelectedSessionId(session.sessionId)}
                              onKeyDown={(event) =>
                                handleSessionRowKeyDown(event, session.sessionId)
                              }
                            >
                              <div className="codex-session-row__left">
                                <input
                                  className="codex-session-row__checkbox"
                                  type="checkbox"
                                  checked={selectedIdSet.has(session.sessionId)}
                                  onChange={() => toggleSession(session.sessionId)}
                                  onClick={(event) => event.stopPropagation()}
                                />
                                <div className="codex-session-row__content">
                                  <span
                                    className="codex-session-row__title"
                                    title={session.title}
                                  >
                                    {session.isFavorite ? (
                                      <Star
                                        size={12}
                                        className="codex-session-row__favorite"
                                        fill="currentColor"
                                      />
                                    ) : null}
                                    <span className="codex-session-row__title-text">
                                      {session.title ||
                                        t('codex.sessionManager.untitled', '未命名会话')}
                                    </span>
                                  </span>
                                  <span className="codex-session-row__meta">
                                    {session.locations
                                      .map((location) => location.instanceName)
                                      .join(' / ')}
                                    {hasRunningLocation
                                      ? t(
                                          'codex.sessionManager.locationRunning',
                                          '（运行中）',
                                        )
                                      : ''}
                                  </span>
                                  <span
                                    className="codex-session-row__meta codex-session-row__session-id"
                                    title={session.sessionId}
                                  >
                                    {t('codex.sessionManager.labels.sessionId', '会话 ID')}:{' '}
                                    {formatSessionId(session.sessionId)}
                                  </span>
                                </div>
                              </div>
                              <div className="codex-session-row__right">
                                <button
                                  className={`codex-session-row__copy-button${
                                    copiedSessionId === session.sessionId ? ' is-copied' : ''
                                  }`}
                                  type="button"
                                  onClick={(event) =>
                                    void handleCopySessionId(event, session.sessionId)
                                  }
                                  title={t('codex.sessionManager.actions.copySessionId', '复制会话 ID')}
                                  aria-label={t(
                                    'codex.sessionManager.actions.copySessionId',
                                    '复制会话 ID',
                                  )}
                                >
                                  {copiedSessionId === session.sessionId ? (
                                    <Check size={14} />
                                  ) : (
                                    <Copy size={14} />
                                  )}
                                </button>
                                {tokenText ? (
                                  <span
                                    className="codex-session-row__tokens"
                                    title={t(
                                      'codex.sessionManager.labels.tokenUsage',
                                      'Token 使用',
                                    )}
                                  >
                                    {tokenText}
                                  </span>
                                ) : null}
                                {!tokenText && isTokenStatsLoading ? (
                                  <span
                                    className="codex-session-row__tokens"
                                    title={t('common.loading', '加载中...')}
                                  >
                                    <RefreshCw size={12} className="icon-spin" />
                                  </span>
                                ) : null}
                                <span className="codex-session-row__time">
                                  {formatRelativeTime(session.updatedAt, t)}
                                </span>
                              </div>
                            </div>
                          );
                        })}
                      </div>
                    ) : null}
                  </section>
                );
              })}
            </div>
          ) : null}
        </aside>

        <div
          className={`codex-session-viewer__resizer${
            activeResizer === 'left' ? ' is-active' : ''
          }`}
          role="separator"
          aria-orientation="vertical"
          onPointerDown={beginResize('left')}
        >
          <GripVertical size={14} />
        </div>

        <main className="codex-session-viewer__timeline">
          {!selectedSession ? (
            <div className="codex-session-viewer__empty codex-session-viewer__empty--large">
              <Folder size={24} />
              <span>{t('codex.sessionManager.viewer.pickSession', '请选择一条会话')}</span>
            </div>
          ) : (
            <>
              <div className="codex-session-viewer__timeline-head">
                <div className="codex-session-viewer__timeline-heading">
                  <h3>{selectedSession.title || t('codex.sessionManager.untitled', '未命名会话')}</h3>
                  <span>
                    {t('codex.sessionManager.viewer.conversationCount', '共 {{count}} 条问答', {
                      count: conversationEvents.length,
                    })}
                  </span>
                </div>
              </div>

              {timeline.warnings.length > 0 ? (
                <div className="codex-session-viewer__warnings">
                  {timeline.warnings.map((warning) => (
                    <div key={warning} className="codex-session-viewer__warning-row">
                      <AlertTriangle size={14} />
                      <span>{warning}</span>
                    </div>
                  ))}
                </div>
              ) : null}

              {loadingTimeline ? (
                <div className="codex-session-viewer__empty codex-session-viewer__empty--large">
                  <RefreshCw size={18} className="icon-spin" />
                  <span>{t('common.loading', '加载中...')}</span>
                </div>
              ) : null}

              {!loadingTimeline && conversationEvents.length === 0 ? (
                <div className="codex-session-viewer__empty codex-session-viewer__empty--large">
                  <Folder size={20} />
                  <span>
                    {t(
                      'codex.sessionManager.viewer.noConversation',
                      '这条会话没有可显示的问答内容',
                    )}
                  </span>
                </div>
              ) : null}

              {!loadingTimeline ? (
                <div className="codex-session-viewer__conversation">
                  {conversationEvents.map((event) => {
                    const expanded = expandedEventIds.includes(event.id);
                    const collapsed = isLongMessage(event) && !expanded;
                    const timestampValue = event.timestamp
                      ? Date.parse(event.timestamp) / 1000
                      : null;
                    const displayBody = getDisplayBody(event);

                    return (
                      <article
                        key={event.id}
                        className={`codex-session-viewer__bubble ${
                          event.kind === 'user_message' ? 'from-user' : 'from-assistant'
                        }${selectedEventId === event.id ? ' selected' : ''}`}
                        onClick={() => setSelectedEventId(event.id)}
                      >
                        <div className="codex-session-viewer__bubble-head">
                          <div className="codex-session-viewer__bubble-label">
                            {event.kind === 'user_message'
                              ? t('codex.sessionManager.viewer.userLabel', '用户')
                              : t('codex.sessionManager.viewer.assistantLabel', '助手')}
                          </div>
                          <span className="codex-session-viewer__bubble-time">
                            {formatAbsoluteTime(timestampValue)}
                          </span>
                        </div>

                        {collapsed ? (
                          <div className="codex-session-viewer__bubble-preview">
                            {previewMessage(event)}
                          </div>
                        ) : (
                          <pre className="codex-session-viewer__bubble-text">{displayBody}</pre>
                        )}

                        {isLongMessage(event) ? (
                          <button
                            type="button"
                            className="codex-session-viewer__toggle"
                            onClick={(clickEvent) => {
                              clickEvent.stopPropagation();
                              setExpandedEventIds((prev) =>
                                prev.includes(event.id)
                                  ? prev.filter((id) => id !== event.id)
                                  : [...prev, event.id],
                              );
                              setSelectedEventId(event.id);
                            }}
                          >
                            {expanded
                              ? t('codex.sessionManager.actions.collapse', '收起')
                              : t('codex.sessionManager.actions.expand', '展开')}
                          </button>
                        ) : null}
                      </article>
                    );
                  })}
                </div>
              ) : null}
            </>
          )}
        </main>

        <div
          className={`codex-session-viewer__resizer${
            activeResizer === 'right' ? ' is-active' : ''
          }`}
          role="separator"
          aria-orientation="vertical"
          onPointerDown={beginResize('right')}
        >
          <GripVertical size={14} />
        </div>

        <aside className="codex-session-viewer__detail">
          {!selectedSession ? (
            <div className="codex-session-viewer__empty codex-session-viewer__empty--large">
              <Folder size={20} />
              <span>{t('codex.sessionManager.viewer.pickSession', '请选择一条会话')}</span>
            </div>
          ) : (
            <>
              <section className="codex-session-viewer__panel">
                <div className="codex-session-viewer__panel-head">
                  <h3>{t('codex.sessionManager.viewer.sessionDetail', '会话详情')}</h3>
                </div>

                <label className="codex-session-viewer__field">
                  <span>{t('codex.sessionManager.viewer.titleLabel', '标题')}</span>
                  <input
                    value={titleDraft}
                    onChange={(event) => setTitleDraft(event.target.value)}
                    placeholder={t(
                      'codex.sessionManager.viewer.titlePlaceholder',
                      '输入新的会话标题',
                    )}
                  />
                </label>

                <button
                  className="btn btn-primary codex-session-manager__action-button"
                  type="button"
                  onClick={() => void handleSaveTitle()}
                  disabled={savingTitle || !titleDraft.trim()}
                >
                  <Save size={14} className={savingTitle ? 'icon-spin' : undefined} />
                  {t('codex.sessionManager.viewer.saveTitle', '保存标题')}
                </button>

                <button
                  className="btn btn-secondary codex-session-manager__action-button"
                  type="button"
                  onClick={() => void handleFavoriteSession()}
                  disabled={favoriting}
                >
                  <Star size={14} className={favoriting ? 'icon-spin' : undefined} />
                  {selectedSession.isFavorite
                    ? t('codex.sessionManager.viewer.favoriteRemoveAction', '取消收藏')
                    : t('codex.sessionManager.viewer.favoriteAction', '收藏')}
                </button>

                <p className="codex-session-viewer__panel-tip">
                  {t(
                    'codex.sessionManager.viewer.saveTip',
                    '保存标题会直接写回原会话文件、session_index.jsonl 和 state_5.sqlite。',
                  )}
                </p>

                <div className="codex-session-viewer__meta-grid">
                  <div>
                    <span>ID</span>
                    <strong>{selectedSession.sessionId}</strong>
                  </div>
                  <div>
                    <span>{t('codex.sessionManager.viewer.updatedAt', '更新时间')}</span>
                    <strong>{formatAbsoluteTime(selectedSession.updatedAt)}</strong>
                  </div>
                  <div>
                    <span>{t('codex.sessionManager.viewer.createdAt', '创建时间')}</span>
                    <strong>{formatAbsoluteTime(selectedSession.createdAt)}</strong>
                  </div>
                  <div>
                    <span>{t('codex.sessionManager.viewer.cwdLabel', '工作目录')}</span>
                    <strong>{selectedSession.cwd || '-'}</strong>
                  </div>
                  <div>
                    <span>{t('codex.sessionManager.viewer.modelProvider', '模型供应商')}</span>
                    <strong>{selectedLocation?.modelProvider || selectedSession.modelProvider || '-'}</strong>
                  </div>
                  <div>
                    <span>{t('codex.sessionManager.viewer.sessionPath', '会话文件')}</span>
                    <strong>{selectedLocation?.sessionPath || selectedSession.sessionPath || '-'}</strong>
                  </div>
                </div>
              </section>

              <section className="codex-session-viewer__panel">
                <div className="codex-session-viewer__panel-head">
                  <h3>{t('codex.sessionManager.viewer.locations', '会话位置')}</h3>
                </div>

                <div className="codex-session-viewer__location-list">
                  {selectedSession.locations.map((location) => (
                    <button
                      key={`${selectedSession.sessionId}:${location.instanceId}`}
                      type="button"
                      className={`codex-session-viewer__location-pill${
                        selectedLocationInstanceId === location.instanceId ? ' active' : ''
                      }`}
                      onClick={() => setSelectedLocationInstanceId(location.instanceId)}
                    >
                      <strong>{location.instanceName}</strong>
                      <span>{location.cwd || '-'}</span>
                    </button>
                  ))}
                </div>
              </section>

              <section className="codex-session-viewer__panel">
                <div className="codex-session-viewer__panel-head">
                  <h3>{t('codex.sessionManager.viewer.rawEvent', '原始事件')}</h3>
                </div>

                {selectedEvent ? (
                  <div className="codex-session-viewer__raw">
                    <div className="codex-session-viewer__raw-meta">
                      <strong>{selectedEvent.title}</strong>
                      <span>{selectedEvent.summary || selectedEvent.kind}</span>
                    </div>
                    <pre>{selectedEvent.raw}</pre>
                  </div>
                ) : (
                  <div className="codex-session-viewer__empty">
                    <Folder size={18} />
                    <span>
                      {t(
                        'codex.sessionManager.viewer.pickEvent',
                        '点击中间的消息可查看原始事件',
                      )}
                    </span>
                  </div>
                )}
              </section>
            </>
          )}
        </aside>
      </div>

      {showRestoreModal ? (
        <div className="modal-overlay" onClick={closeRestoreModal}>
          <div
            className="modal codex-session-restore-modal"
            onClick={(event) => event.stopPropagation()}
          >
            <div className="modal-header">
              <h2>{t('codex.sessionManager.restoreModal.title', '恢复会话')}</h2>
              <button
                className="modal-close"
                type="button"
                onClick={closeRestoreModal}
                disabled={restoring}
                aria-label={t('common.close', '关闭')}
              >
                <X size={18} />
              </button>
            </div>

            <div className="modal-body">
              <ModalErrorMessage
                message={restoreModalError}
                scrollKey={restoreModalErrorScrollKey}
              />

              {loadingTrash ? (
                <div className="codex-session-restore-modal__empty">
                  <h3>{t('common.loading', '加载中...')}</h3>
                </div>
              ) : null}

              {!loadingTrash && trashedSessions.length === 0 ? (
                <div className="codex-session-restore-modal__empty">
                  <Folder size={36} className="empty-icon" />
                  <h3>{t('codex.sessionManager.restoreModal.emptyTitle', '回收区里还没有会话')}</h3>
                  <p>
                    {t(
                      'codex.sessionManager.restoreModal.emptyDesc',
                      '移到回收区的会话会显示在这里。',
                    )}
                  </p>
                </div>
              ) : null}

              {!loadingTrash && trashedSessions.length > 0 ? (
                <div className="codex-session-restore-list">
                  {trashedSessions.map((session) => (
                    <label className="codex-session-restore-row" key={session.sessionId}>
                      <div className="codex-session-restore-row__left">
                        <input
                          className="codex-session-row__checkbox"
                          type="checkbox"
                          checked={selectedTrashIdSet.has(session.sessionId)}
                          onChange={() =>
                            setSelectedTrashIds((prev) =>
                              prev.includes(session.sessionId)
                                ? prev.filter((id) => id !== session.sessionId)
                                : [...prev, session.sessionId],
                            )
                          }
                        />
                        <div className="codex-session-restore-row__content">
                          <span
                            className="codex-session-restore-row__title"
                            title={session.title}
                          >
                            {session.title || t('codex.sessionManager.untitled', '未命名会话')}
                          </span>
                          <span className="codex-session-restore-row__meta">
                            {session.locations.map((location) => location.instanceName).join(' / ')}
                          </span>
                          <span className="codex-session-restore-row__meta codex-session-restore-row__cwd">
                            {session.cwd}
                          </span>
                        </div>
                      </div>
                      <span className="codex-session-row__time">
                        {formatRelativeTime(session.deletedAt, t)}
                      </span>
                    </label>
                  ))}
                </div>
              ) : null}
            </div>

            <div className="modal-footer">
              <button
                className="btn btn-secondary"
                type="button"
                onClick={closeRestoreModal}
                disabled={restoring}
              >
                {t('common.cancel', '取消')}
              </button>
              <button
                className="btn btn-primary"
                type="button"
                onClick={() => void handleRestoreFromTrash()}
                disabled={restoring || loadingTrash || selectedTrashIds.length === 0}
              >
                <RotateCcw size={14} className={restoring ? 'icon-spin' : undefined} />
                {t('codex.sessionManager.restoreModal.restoreAction', '恢复选中会话')} (
                {selectedTrashIds.length})
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </section>
  );
}
