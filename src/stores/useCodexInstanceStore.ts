import * as codexInstanceService from '../services/codexInstanceService';
import type {
  CodexSessionVisibilityRepairSummary,
  CodexInstanceThreadSyncSummary,
  CodexSessionRecord,
  CodexSessionViewerRecord,
  CodexSessionTrashSummary,
  CodexTrashedSessionRecord,
  CodexSessionRestoreSummary,
  CodexSessionTimeline,
  CodexSessionTitleUpdateResult,
  CodexSessionFavoriteResult,
} from '../types/codex';
import { createInstanceStore, type InstanceStoreState } from './createInstanceStore';

type CodexInstanceStoreState = InstanceStoreState & {
  syncThreadsAcrossInstances: () => Promise<CodexInstanceThreadSyncSummary>;
  repairSessionVisibilityAcrossInstances: () => Promise<CodexSessionVisibilityRepairSummary>;
  listSessionsAcrossInstances: () => Promise<CodexSessionRecord[]>;
  listSessionsForViewer: () => Promise<CodexSessionViewerRecord[]>;
  getSessionTimeline: (sessionId: string, instanceId?: string | null) => Promise<CodexSessionTimeline>;
  updateSessionTitle: (sessionId: string, title: string) => Promise<CodexSessionTitleUpdateResult>;
  favoriteSession: (sessionId: string) => Promise<CodexSessionFavoriteResult>;
  unfavoriteSession: (sessionId: string) => Promise<CodexSessionFavoriteResult>;
  moveSessionsToTrashAcrossInstances: (sessionIds: string[]) => Promise<CodexSessionTrashSummary>;
  listTrashedSessionsAcrossInstances: () => Promise<CodexTrashedSessionRecord[]>;
  restoreSessionsFromTrashAcrossInstances: (sessionIds: string[]) => Promise<CodexSessionRestoreSummary>;
};

type CodexInstanceStoreHook = {
  (): CodexInstanceStoreState;
  <T>(selector: (state: CodexInstanceStoreState) => T): T;
  getState: () => CodexInstanceStoreState;
  setState: (partial: Partial<CodexInstanceStoreState>) => void;
};

const baseStore = createInstanceStore(codexInstanceService, 'agtools.codex.instances.cache');
const typedBaseStore = baseStore as unknown as CodexInstanceStoreHook;

const syncThreadsAcrossInstances = async (): Promise<CodexInstanceThreadSyncSummary> => {
  const summary = await codexInstanceService.syncThreadsAcrossInstances();
  await typedBaseStore.getState().fetchInstances();
  return summary;
};

const repairSessionVisibilityAcrossInstances = async (): Promise<CodexSessionVisibilityRepairSummary> => {
  const summary = await codexInstanceService.repairSessionVisibilityAcrossInstances();
  await typedBaseStore.getState().fetchInstances();
  return summary;
};

const listSessionsAcrossInstances = async (): Promise<CodexSessionRecord[]> => {
  return await codexInstanceService.listSessionsAcrossInstances();
};

const listSessionsForViewer = async (): Promise<CodexSessionViewerRecord[]> => {
  return await codexInstanceService.listSessionsForViewer();
};

const getSessionTimeline = async (
  sessionId: string,
  instanceId?: string | null,
): Promise<CodexSessionTimeline> => {
  return await codexInstanceService.getSessionTimeline(sessionId, instanceId);
};

const updateSessionTitle = async (
  sessionId: string,
  title: string,
): Promise<CodexSessionTitleUpdateResult> => {
  return await codexInstanceService.updateSessionTitle(sessionId, title);
};

const favoriteSession = async (
  sessionId: string,
): Promise<CodexSessionFavoriteResult> => {
  return await codexInstanceService.favoriteSession(sessionId);
};

const unfavoriteSession = async (
  sessionId: string,
): Promise<CodexSessionFavoriteResult> => {
  return await codexInstanceService.unfavoriteSession(sessionId);
};

const moveSessionsToTrashAcrossInstances = async (
  sessionIds: string[],
): Promise<CodexSessionTrashSummary> => {
  const summary = await codexInstanceService.moveSessionsToTrashAcrossInstances(sessionIds);
  await typedBaseStore.getState().fetchInstances();
  return summary;
};

const listTrashedSessionsAcrossInstances = async (): Promise<CodexTrashedSessionRecord[]> => {
  return await codexInstanceService.listTrashedSessionsAcrossInstances();
};

const restoreSessionsFromTrashAcrossInstances = async (
  sessionIds: string[],
): Promise<CodexSessionRestoreSummary> => {
  const summary = await codexInstanceService.restoreSessionsFromTrashAcrossInstances(sessionIds);
  await typedBaseStore.getState().fetchInstances();
  return summary;
};

typedBaseStore.setState({
  syncThreadsAcrossInstances,
  repairSessionVisibilityAcrossInstances,
  listSessionsAcrossInstances,
  listSessionsForViewer,
  getSessionTimeline,
  updateSessionTitle,
  favoriteSession,
  unfavoriteSession,
  moveSessionsToTrashAcrossInstances,
  listTrashedSessionsAcrossInstances,
  restoreSessionsFromTrashAcrossInstances,
});

export const useCodexInstanceStore = typedBaseStore;
