//! 自动回复过程中两个平台共用的带副作用动作。
//!
//! 和 [`crate::rpa::conversation`] 分开放，是因为那边刻意保持纯判断——它的护栏
//! 逻辑全靠单测兜着，掺进读写库就得先搭一套数据目录才跑得起来。这边则相反，
//! 每个函数都在写库或等时钟，但它们同样是平台无关的：BOSS 和猎聘挂起会话的
//! 字段口径、记流水的时机、消项的判据必须一致，否则待办列表里两个平台的
//! 条目长得不一样，节流额度也会各算各的。

use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::ReplyPollingConfig;
use crate::dao::model::{AutoReplyAction, ManualReviewReason, ManualReviewRecord};
use crate::dao::{auto_reply_log_dao, manual_review_dao};
use crate::logger;
use crate::rpa::common::ChatMessage;
use crate::rpa::conversation::{self, ConversationContext, ReplyLimits, ReviewDraft};
use crate::rpa::polling;
use crate::rpa::run_flow::is_job_task_stop_requested;

/// 记一次自动发送。
///
/// 写失败只警告：消息已经发出去了，这时候往上抛错只会让调用方以为没发成功，
/// 进而在下一轮重复发送
pub fn record_auto_send(
    platform: &str,
    conversation_id: &str,
    job_id: &str,
    action: AutoReplyAction,
    chars: usize,
) {
    if let Err(error) = auto_reply_log_dao::record(platform, conversation_id, job_id, action, chars)
    {
        let _ = logger::warning(format!("记录自动发送流水失败: {error}"));
    }
}

/// 把会话挂进待人工处理列表。
///
/// 写失败必须结束当前会话，由平台循环记录错误并继续其他会话。
/// 此时只能保证本轮不发送，不能声称已持久挂起。
pub fn hold_for_review(
    platform: &str,
    conversation_id: &str,
    draft: ReviewDraft,
    reason: ManualReviewReason,
    detail: String,
) -> Result<()> {
    let request = draft.to_request(platform, conversation_id, reason, detail);
    if reason == ManualReviewReason::MissingJobId {
        // 重试读取标识不是新的人工事件，不能覆盖手工回复所参照的挂起时间。
        return hold_if_absent_with(
            || manual_review_dao::get(platform, conversation_id),
            || manual_review_dao::upsert(request),
        )
        .context("读取或持久化会话标识缺失挂起失败，本轮不发送；下轮挂起无法保证");
    }
    manual_review_dao::upsert(request).context("持久化人工挂起失败，本轮不发送；下轮挂起无法保证")
}

fn hold_if_absent_with(
    get: impl FnOnce() -> Result<Option<ManualReviewRecord>>,
    insert: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if get()?.is_none() {
        insert()?;
    }
    Ok(())
}

/// 先按手工回复/滚动额度恢复消项，再判断是否仍挂起。读取失败不是“没有挂起”。
/// messages 必须带真实时间戳；DOM 抓取时刻不能作为手工回复的证据。
/// log_conversation_id 始终使用正常发送流水主键，即使挂起记录使用 BOSS 卡片标识。
pub fn pending_review(
    platform: &str,
    conversation_id: &str,
    log_conversation_id: &str,
    messages: &[ChatMessage],
    auto_replies_in_window: usize,
    limits: &ReplyLimits,
) -> Result<bool> {
    pending_review_with(
        || manual_review_dao::get(platform, conversation_id),
        || auto_reply_log_dao::last_sent_at(platform, log_conversation_id),
        || manual_review_dao::resolve(platform, conversation_id),
        messages,
        auto_replies_in_window,
        limits,
    )
    .context("检查人工挂起失败，本会话不自动处理")
}

fn pending_review_with(
    get: impl Fn() -> Result<Option<ManualReviewRecord>>,
    last_sent: impl FnOnce() -> Result<Option<i64>>,
    resolve: impl FnOnce() -> Result<bool>,
    messages: &[ChatMessage],
    auto_replies_in_window: usize,
    limits: &ReplyLimits,
) -> Result<bool> {
    let Some(held) = get()? else {
        return Ok(false);
    };
    // 不得把流水读取错误当成 None，否则工具自己的发送会被误判为人手处理。
    let last_auto = last_sent()?;
    if conversation::review_can_resume(&held, messages, last_auto, auto_replies_in_window, limits) {
        resolve()?;
        // 删除失败或并发重新挂起都不能放行。
        return Ok(get()?.is_some());
    }
    Ok(true)
}

/// 等待/模型调用之后、每个发送动作之前重新检查，不用旧消息自动解除新挂起。
pub fn outbound_allowed(platform: &str, conversation_ids: &[&str]) -> Result<bool> {
    outbound_allowed_with(
        is_job_task_stop_requested,
        |id| manual_review_dao::get(platform, id),
        conversation_ids,
    )
}

fn outbound_allowed_with(
    stopped: impl Fn() -> bool,
    get: impl Fn(&str) -> Result<Option<ManualReviewRecord>>,
    conversation_ids: &[&str],
) -> Result<bool> {
    if stopped() {
        return Ok(false);
    }
    for id in conversation_ids {
        if get(id)
            .context("发送前读取人工挂起失败，本次不发送")?
            .is_some()
        {
            return Ok(false);
        }
    }
    Ok(!stopped())
}

/// 发送之前补足「像人在打字」的那段间隔。返回 false 表示等待途中任务被停止。
///
/// 秒回比慢回更像机器人：HR 不指望你三秒回，但会注意到你每次都三秒回。
/// 真正要凑的是「对方发出到我方回复」的总间隔，而轮询周期本身通常已经把它
/// 填满了，所以绝大多数情况下这里一秒都不等
pub async fn wait_before_reply(
    polling_config: &ReplyPollingConfig,
    context: &ConversationContext,
) -> bool {
    let elapsed_ms = context
        .last_received()
        .map(|message| chrono::Local::now().timestamp_millis() - message.time);
    let seconds = stretch_by_persona(polling::humanize_delay_seconds_now(
        polling_config,
        elapsed_ms,
    ));
    if seconds == 0 {
        return !is_job_task_stop_requested();
    }

    let _ = logger::info(format!("等待 {seconds} 秒后回复，避免秒回"));
    // 最长可能等两分钟，用户点了停止不该干等着
    for _ in 0..seconds {
        if is_job_task_stop_requested() {
            return false;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    !is_job_task_stop_requested()
}

/// 按当日人格的节奏系数拉长等待。
///
/// 这里刻意只放大、不缩短：用户在轮询配置里设的等待区间是他要求的下限，
/// 拟人化可以让今天的自己回得更慢一些，但没有资格替他回得更快
fn stretch_by_persona(seconds: u64) -> u64 {
    match crate::rpa::humanize::current_persona() {
        Some(persona) => ((seconds as f64) * persona.pace.max(1.0)).round() as u64,
        None => seconds,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dao::store::JsonStore;
    use crate::rpa::conversation::{ReplyAction, ReplyDecision};

    fn limits() -> ReplyLimits {
        ReplyLimits {
            max_auto_replies: 5,
            auto_reply_window_hours: 24,
            max_reply_chars: 200,
            allow_auto_send_resume: true,
            dry_run: false,
        }
    }

    fn held(reason: ManualReviewReason) -> ManualReviewRecord {
        ManualReviewRecord {
            id: "boss:c1".into(),
            platform: "boss".into(),
            conversation_id: "c1".into(),
            job_id: String::new(),
            job_name: String::new(),
            company_name: String::new(),
            reason,
            detail: "test hold".into(),
            last_message: String::new(),
            created_at: 100,
            updated_at: 100,
            hit_count: 1,
        }
    }

    fn message(received: bool, time: i64) -> ChatMessage {
        ChatMessage {
            mid: time,
            received,
            text: "方便聊聊吗".into(),
            time,
            from_name: String::new(),
        }
    }

    fn check(
        store: &JsonStore<ManualReviewRecord>,
        messages: &[ChatMessage],
        last_auto: Result<Option<i64>>,
        count: usize,
    ) -> Result<bool> {
        pending_review_with(
            || store.get_by_id("boss:c1"),
            || last_auto,
            || store.delete_by_id("boss:c1"),
            messages,
            count,
            &limits(),
        )
    }

    #[test]
    fn benign_followup_preserves_every_non_quota_hold_without_refreshing_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonStore::new(dir.path(), "reviews.json").unwrap();
        for reason in [
            ManualReviewReason::RiskKeyword,
            ManualReviewReason::VetRejected,
            ManualReviewReason::MissingJobId,
            ManualReviewReason::ModelEscalation,
        ] {
            let record = held(reason);
            store.replace_all(vec![record.clone()]).unwrap();
            assert!(check(&store, &[message(true, 300)], Ok(None), 0).unwrap());
            assert_eq!(store.get_by_id("boss:c1").unwrap(), Some(record));
        }
    }

    #[test]
    fn missing_id_retry_preserves_manual_reply_evidence_until_id_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonStore::new(dir.path(), "reviews.json").unwrap();
        hold_if_absent_with(
            || store.get_by_id("boss:c1"),
            || store.insert(held(ManualReviewReason::MissingJobId)),
        )
        .unwrap();
        let messages = [message(false, 200), message(true, 250)];
        hold_if_absent_with(
            || store.get_by_id("boss:c1"),
            || {
                let mut retry = held(ManualReviewReason::MissingJobId);
                retry.updated_at = 300;
                store.replace_all(vec![retry])
            },
        )
        .unwrap();
        assert_eq!(store.get_by_id("boss:c1").unwrap().unwrap().updated_at, 100);
        assert!(!check(&store, &messages, Ok(None), 0).unwrap());
        assert!(store.load_all().unwrap().is_empty());
    }

    #[test]
    fn missing_id_hold_read_failure_does_not_upsert() {
        assert!(hold_if_absent_with(
            || Err(anyhow::anyhow!("review read failed")),
            || panic!("must not overwrite unreadable hold"),
        )
        .is_err());
    }

    #[test]
    fn manual_but_not_automatic_reply_resolves_and_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonStore::new(dir.path(), "reviews.json").unwrap();
        store.insert(held(ManualReviewReason::RiskKeyword)).unwrap();
        let messages = [message(false, 200), message(true, 300)];
        assert!(check(&store, &messages, Ok(Some(200)), 0).unwrap());
        assert!(!check(&store, &messages, Ok(Some(150)), 0).unwrap());
        let reopened = JsonStore::<ManualReviewRecord>::new(dir.path(), "reviews.json").unwrap();
        assert!(reopened.load_all().unwrap().is_empty());
    }

    #[test]
    fn quota_hold_recovers_only_when_window_count_falls_below_limit() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonStore::new(dir.path(), "reviews.json").unwrap();
        store
            .insert(held(ManualReviewReason::ThrottleExhausted))
            .unwrap();
        assert!(check(&store, &[], Ok(Some(200)), 5).unwrap());
        assert!(!check(&store, &[], Ok(Some(200)), 4).unwrap());
    }

    #[test]
    fn send_log_read_failure_never_misclassifies_own_message_as_manual() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonStore::new(dir.path(), "reviews.json").unwrap();
        store.insert(held(ManualReviewReason::RiskKeyword)).unwrap();
        assert!(check(
            &store,
            &[message(false, 200)],
            Err(anyhow::anyhow!("unreadable send log")),
            0,
        )
        .is_err());
        assert!(store.get_by_id("boss:c1").unwrap().is_some());
    }

    #[test]
    fn review_read_and_resolution_write_failures_fail_closed() {
        assert!(pending_review_with(
            || Err(anyhow::anyhow!("read failed")),
            || panic!("must not read log"),
            || panic!("must not resolve"),
            &[],
            0,
            &limits(),
        )
        .is_err());
        assert!(pending_review_with(
            || Ok(Some(held(ManualReviewReason::RiskKeyword))),
            || Ok(None),
            || Err(anyhow::anyhow!("write failed")),
            &[message(false, 200)],
            0,
            &limits(),
        )
        .is_err());
        assert!(pending_review_with(
            || Ok(Some(held(ManualReviewReason::RiskKeyword))),
            || Ok(None),
            || Ok(false),
            &[message(false, 200)],
            0,
            &limits(),
        )
        .unwrap());
    }

    #[test]
    fn absent_hold_preserves_normal_path_without_send_log_lookup() {
        assert!(!pending_review_with(
            || Ok(None),
            || panic!("no hold requires no lookup"),
            || panic!("no hold to resolve"),
            &[message(true, 200)],
            0,
            &limits(),
        )
        .unwrap());
    }

    #[test]
    fn presend_recheck_blocks_new_hold_stop_and_read_failure() {
        assert!(outbound_allowed_with(|| false, |_| Ok(None), &["c1"]).unwrap());
        assert!(!outbound_allowed_with(|| true, |_| panic!("stopped"), &["c1"]).unwrap());
        assert!(!outbound_allowed_with(
            || false,
            |_| Ok(Some(held(ManualReviewReason::RiskKeyword))),
            &["c1"],
        )
        .unwrap());
        assert!(
            outbound_allowed_with(|| false, |_| Err(anyhow::anyhow!("read failed")), &["c1"],)
                .is_err()
        );
        // A recovered BOSS job id must still honor the original card-key hold.
        assert!(!outbound_allowed_with(
            || false,
            |id| Ok((id == "card").then(|| held(ManualReviewReason::MissingJobId))),
            &["job", "card"],
        )
        .unwrap());
    }

    #[test]
    fn model_escalation_is_distinct_from_skip_and_roundtrips() {
        for action in [
            ReplyAction::Skip,
            ReplyAction::Reply,
            ReplyAction::ReplyAndSendResume,
        ] {
            assert!(conversation::decision_review(&ReplyDecision {
                action,
                reply: String::new(),
                reason: "无需人工".into(),
                confidence: 90,
            })
            .is_none());
        }
        let decision = ReplyDecision {
            action: ReplyAction::Escalate,
            reply: String::new(),
            reason: "需要你确认面试安排".into(),
            confidence: 90,
        };
        assert_eq!(
            conversation::decision_review(&decision),
            Some((ManualReviewReason::ModelEscalation, decision.reason.clone()))
        );
        let json = serde_json::to_string(&held(ManualReviewReason::ModelEscalation)).unwrap();
        assert!(json.contains("\"model_escalation\""));
        assert_eq!(
            serde_json::from_str::<ManualReviewRecord>(&json)
                .unwrap()
                .reason,
            ManualReviewReason::ModelEscalation,
        );
    }

    #[test]
    fn explicit_resolve_and_clear_restore_eligibility_without_a_dismiss_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonStore::new(dir.path(), "reviews.json").unwrap();
        store
            .insert(held(ManualReviewReason::ModelEscalation))
            .unwrap();
        assert!(check(&store, &[], Ok(None), 0).unwrap());
        store.delete_by_id("boss:c1").unwrap();
        assert!(!check(&store, &[], Ok(None), 0).unwrap());
        store.insert(held(ManualReviewReason::RiskKeyword)).unwrap();
        store.replace_all(Vec::new()).unwrap();
        assert!(!check(&store, &[], Ok(None), 0).unwrap());
    }

    #[test]
    fn malformed_review_file_fails_closed_without_silently_clearing_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonStore::<ManualReviewRecord>::new(dir.path(), "reviews.json").unwrap();
        let path = dir.path().join("data/reviews.json");
        std::fs::write(&path, "{broken").unwrap();
        assert!(check(&store, &[message(false, 200)], Ok(None), 0).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{broken");
    }
}
