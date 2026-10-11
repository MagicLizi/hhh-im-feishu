//! 平台请求记账：插件替宿主办一件事（一次宿主调用）时，数一共向飞书发了几次请求；办完随结果报给宿主
//! （协议里的 `platformRequests`），宿主按实际次数记进平台额度。
//!
//! 飞书适配器在唯一的查询出口（`query_data`）发请求前各记一次 `request`；不在数的任务里（没包在 [`counted`]
//! 里的调用）时什么也不做。
//!
//! 从 HHH 的 hhh-im 搬来时去掉了“门”：HHH 内置时调度器给多页读取装一道门，额度不够就让请求等着；插件不排队、
//! 也不等，只数数，额度由宿主按报上来的次数管。所以 `request` 永不阻塞、永不报错。

use std::cell::Cell;
use std::future::Future;

tokio::task_local! {
    // task_local（任务局部变量）：每个 tokio 任务各有一份，同时进行的几次调用各数各的、互不串。
    // `Cell` 是可以在只读借用里改的小格子；它只在这个任务里用，不跨线程共享。
    static SENT: Cell<u32>;
}

/// 适配器每发一次查询请求前记一次：在 [`counted`] 里时数一次，否则什么也不做。
pub(crate) fn request() {
    // `try_with` 是“这个任务里有计数器就用它”：没有（不在数的任务里）时返回错误，这里不在乎。
    let _ = SENT.try_with(|sent| sent.set(sent.get().saturating_add(1)));
}

/// 跑一段调用，同时数它向平台发了几次请求（不等额度，办完由调用方把次数报给宿主）。
///
/// # Examples
///
/// ```
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let (answer, sent) = hhh_im_feishu::metering::counted(async { 42 }).await;
/// assert_eq!((answer, sent), (42, 0));
/// # }
/// ```
pub async fn counted<F: Future>(future: F) -> (F::Output, u32) {
    SENT.scope(Cell::new(0), async move {
        let output = future.await;
        (output, SENT.with(Cell::get))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    // 同时进行的两段调用各数各的；不在数的任务里发的请求不算进任何一段。
    #[tokio::test]
    async fn each_call_counts_only_its_own_requests() {
        request();
        let one = counted(async {
            request();
            tokio::task::yield_now().await;
            request();
        });
        let two = counted(async {
            request();
        });
        let ((_, a), (_, b)) = tokio::join!(one, two);
        assert_eq!((a, b), (2, 1));
    }
}
