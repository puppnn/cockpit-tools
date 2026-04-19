use std::future::Future;
use std::time::Duration;

use crate::modules::logger;

pub const ACCOUNT_REFRESH_RETRY_DELAY_SECS: u64 = 10;

pub async fn retry_once_with_delay<T, F, Fut>(
    scope: &str,
    account_id: &str,
    mut operation: F,
) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, String>>,
{
    match operation().await {
        Ok(value) => Ok(value),
        Err(first_error) => {
            logger::log_warn(&format!(
                "[{}] 首次刷新失败，{} 秒后重试: account_id={}, error={}",
                scope, ACCOUNT_REFRESH_RETRY_DELAY_SECS, account_id, first_error
            ));
            tokio::time::sleep(Duration::from_secs(ACCOUNT_REFRESH_RETRY_DELAY_SECS)).await;

            match operation().await {
                Ok(value) => {
                    logger::log_info(&format!(
                        "[{}] 重试刷新成功: account_id={}",
                        scope, account_id
                    ));
                    Ok(value)
                }
                Err(second_error) => {
                    logger::log_warn(&format!(
                        "[{}] 重试后仍失败: account_id={}, first_error={}, second_error={}",
                        scope, account_id, first_error, second_error
                    ));
                    Err(second_error)
                }
            }
        }
    }
}
