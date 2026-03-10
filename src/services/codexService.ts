import { invoke } from '@tauri-apps/api/core';
import { CodexAccount, CodexQuota } from '../types/codex';

export interface CodexOAuthLoginStartResponse {
  loginId: string;
  authUrl: string;
}

/** 列出所有 Codex 账号 */
export async function listCodexAccounts(): Promise<CodexAccount[]> {
  return await invoke('list_codex_accounts');
}

/** 获取当前激活的 Codex 账号 */
export async function getCurrentCodexAccount(): Promise<CodexAccount | null> {
  return await invoke('get_current_codex_account');
}

/** 刷新 Codex 账号资料（团队名/结构） */
export async function refreshCodexAccountProfile(accountId: string): Promise<CodexAccount> {
  return await invoke('refresh_codex_account_profile', { accountId });
}

/** 切换 Codex 账号 */
export async function switchCodexAccount(accountId: string): Promise<CodexAccount> {
  return await invoke('switch_codex_account', { accountId });
}

/** 删除 Codex 账号 */
export async function deleteCodexAccount(accountId: string): Promise<void> {
  return await invoke('delete_codex_account', { accountId });
}

/** 批量删除 Codex 账号 */
export async function deleteCodexAccounts(accountIds: string[]): Promise<void> {
  return await invoke('delete_codex_accounts', { accountIds });
}

/** 从本地 auth.json 导入账号 */
export async function importCodexFromLocal(): Promise<CodexAccount> {
  return await invoke('import_codex_from_local');
}

/** 从 JSON 字符串导入账号 */
export async function importCodexFromJson(jsonContent: string): Promise<CodexAccount[]> {
  return await invoke('import_codex_from_json', { jsonContent });
}

/** 导出 Codex 账号 */
export async function exportCodexAccounts(accountIds: string[]): Promise<string> {
  return await invoke('export_codex_accounts', { accountIds });
}

export interface CodexFileImportResult {
  imported: CodexAccount[];
  failed: { email: string; error: string }[];
}

/** 从本地文件导入 Codex 账号 */
export async function importCodexFromFiles(filePaths: string[]): Promise<CodexFileImportResult> {
  return await invoke('import_codex_from_files', { filePaths });
}

/** 刷新单个账号配额 */
export async function refreshCodexQuota(accountId: string): Promise<CodexQuota> {
  return await invoke('refresh_codex_quota', { accountId });
}

/** 刷新所有账号配额 */
export async function refreshAllCodexQuotas(): Promise<number> {
  return await invoke('refresh_all_codex_quotas');
}

/** 新 OAuth 流程：开始登录 */
export async function startCodexOAuthLogin(): Promise<CodexOAuthLoginStartResponse> {
  return await invoke('codex_oauth_login_start');
}

/** 新 OAuth 流程：完成登录 */
export async function completeCodexOAuthLogin(loginId: string): Promise<CodexAccount> {
  return await invoke('codex_oauth_login_completed', { loginId });
}

/** 新 OAuth 流程：取消登录 */
export async function cancelCodexOAuthLogin(loginId?: string): Promise<void> {
  return await invoke('codex_oauth_login_cancel', { loginId: loginId ?? null });
}

/** 通过 Token 添加账号 */
export async function addCodexAccountWithToken(
  idToken: string,
  accessToken: string,
  refreshToken?: string
): Promise<CodexAccount> {
  return await invoke('add_codex_account_with_token', {
    idToken,
    accessToken,
    refreshToken: refreshToken ?? null,
  });
}

/** 检查 Codex OAuth 端口是否被占用 */
export async function isCodexOAuthPortInUse(): Promise<boolean> {
  return await invoke('is_codex_oauth_port_in_use');
}

/** 关闭占用 Codex OAuth 端口的进程 */
export async function closeCodexOAuthPort(): Promise<number> {
  return await invoke('close_codex_oauth_port');
}

export async function updateCodexAccountTags(accountId: string, tags: string[]): Promise<CodexAccount> {
  return await invoke('update_codex_account_tags', { accountId, tags });
}
