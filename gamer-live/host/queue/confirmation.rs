use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Confirmation {
    pub state: String,
    pub started_at: i64,
    pub deadline_at: i64,
    #[serde(default)]
    pub confirmation_deadline_at: Option<i64>,
    pub decided_at: Option<i64>,
    pub decision_event_id: Option<String>,
}
impl Confirmation {
    pub(super) fn declined(&self) -> bool {
        ["cancelled", "timeout"].contains(&self.state.as_str())
    }
    fn accepts_time(&self, event: &LiveEvent) -> bool {
        let received = chrono::DateTime::parse_from_rfc3339(&event.received_at)
            .ok()
            .map(|at| at.timestamp_millis());
        received.is_some_and(|at| at >= self.started_at)
            && event.occurred_at.is_none_or(|at| {
                if at >= 100_000_000_000 {
                    at >= self.started_at
                } else {
                    at >= self.started_at / 1000
                }
            })
    }
}
impl Queue {
    pub async fn begin_scan(
        &self,
        run_id: &str,
        device: &str,
        package: &str,
        timeout_secs: u64,
    ) -> Result<Value> {
        let _notices = NoticeGuard(self);
        ensure!((1..=300).contains(&timeout_secs), "扫码等待须为1到300秒");
        let mut s = self.state.lock().await;
        let index = Self::confirmation_index(&s, run_id, device, package)?;
        if s.items[index].confirmation.is_none() {
            let mut n = s.clone();
            n.items[index].phase = "awaiting_scan".into();
            n.items[index].confirmation = Some(Confirmation {
                state: "awaiting_scan".into(),
                started_at: now(),
                deadline_at: now() + timeout_secs as i64 * 1000,
                confirmation_deadline_at: None,
                decided_at: None,
                decision_event_id: None,
            });
            self.commit(&mut s, n)?;
        }
        self.expire_confirmation(&mut s, index)?;
        Ok(Self::confirmation_response(&s, index))
    }
    pub async fn finish_rescan(&self, run_id: &str, device: &str, package: &str) -> Result<Value> {
        let _notices = NoticeGuard(self);
        let mut s = self.state.lock().await;
        let index = Self::confirmation_index(&s, run_id, device, package)?;
        self.expire_confirmation(&mut s, index)?;
        if s.items[index]
            .confirmation
            .as_ref()
            .is_some_and(|c| c.state == "rescan_requested")
        {
            let mut n = s.clone();
            let c = n.items[index].confirmation.as_mut().unwrap();
            c.state = "awaiting_scan".into();
            c.started_at = now();
            c.decided_at = None;
            c.decision_event_id = None;
            // Keep the deadline; repeated QR requests never buy more time.
            n.items[index].phase = "awaiting_scan".into();
            self.commit(&mut s, n)?;
        }
        Ok(Self::confirmation_response(&s, index))
    }
    fn confirmation_index(s: &State, run_id: &str, device: &str, package: &str) -> Result<usize> {
        let index = s
            .items
            .iter()
            .position(|i| i.run_id.as_deref() == Some(run_id))
            .context("LIVE_CONFIRMATION_UNOWNED_RUN: 运行不属于此互动队列")?;
        let item = &s.items[index];
        ensure!(
            item.target.device_id == device && item.target.package_id == package,
            "LIVE_CONFIRMATION_TARGET_MISMATCH: 执行目标不匹配"
        );
        ensure!(
            item.active()
                && item.state != "review"
                && item.phase != "cleanup"
                && !item.stop_requested,
            "LIVE_CONFIRMATION_UNAVAILABLE: 任务已停止或不在主执行阶段"
        );
        ensure!(item.confirmation_spec.is_some(), "此规则未配置本人确认");
        Ok(index)
    }
    fn confirmation_response(s: &State, index: usize) -> Value {
        let item = &s.items[index];
        let confirmation = item.confirmation.as_ref();
        json!({"status":confirmation.map(|c|match c.state.as_str() { "awaiting_confirmation" => "waiting", "awaiting_scan" => "scanning", "rescan_requested" => "rescan", _ => &c.state }).unwrap_or("not_started"),
            "item_id":item.id,"deadline_at":confirmation.map(|c|c.deadline_at),
            "viewer":item.event["message"]["actor"]["name"],
            "open_id":item.billing.as_ref().and_then(|b|s.users.get(&b.user_id)).map(|u|u.open_id.as_str())})
    }
    pub async fn begin_confirmation(
        &self,
        run_id: &str,
        device: &str,
        package: &str,
        timeout_secs: u64,
    ) -> Result<Value> {
        let _notices = NoticeGuard(self);
        ensure!(
            timeout_secs > 0 && timeout_secs <= 300,
            "确认等待须为1到300秒"
        );
        let mut s = self.state.lock().await;
        let index = Self::confirmation_index(&s, run_id, device, package)?;
        if s.items[index].confirmation.is_none() {
            let mut n = s.clone();
            let seconds = timeout_secs
                .min(
                    n.items[index]
                        .confirmation_spec
                        .as_ref()
                        .unwrap()
                        .timeout_secs,
                )
                .min(300);
            ensure!(seconds > 0, "确认等待配置无效");
            let deadline = now() + seconds as i64 * 1000;
            n.items[index].phase = "awaiting_confirmation".into();
            n.items[index].confirmation = Some(Confirmation {
                state: "awaiting_confirmation".into(),
                started_at: now(),
                deadline_at: deadline,
                confirmation_deadline_at: Some(deadline),
                decided_at: None,
                decision_event_id: None,
            });
            self.commit(&mut s, n)?;
        }
        // A visual login-ready observation may race cancellation, timeout or a
        // new QR request. Only the live scan state can open a fresh gate.
        self.expire_confirmation(&mut s, index)?;
        if s.items[index]
            .confirmation
            .as_ref()
            .is_some_and(|c| c.state == "awaiting_scan")
        {
            let mut n = s.clone();
            let seconds = timeout_secs.min(
                n.items[index]
                    .confirmation_spec
                    .as_ref()
                    .unwrap()
                    .timeout_secs,
            );
            let c = n.items[index].confirmation.as_mut().unwrap();
            let deadline = *c
                .confirmation_deadline_at
                .get_or_insert_with(|| now() + seconds as i64 * 1000);
            c.state = "awaiting_confirmation".into();
            c.started_at = now();
            c.deadline_at = deadline;
            c.decided_at = None;
            c.decision_event_id = None;
            n.items[index].phase = "awaiting_confirmation".into();
            self.commit(&mut s, n)?;
        }
        self.expire_confirmation(&mut s, index)?;
        Ok(Self::confirmation_response(&s, index))
    }
    pub async fn poll_confirmation(
        &self,
        run_id: &str,
        device: &str,
        package: &str,
    ) -> Result<Value> {
        let _notices = NoticeGuard(self);
        let mut s = self.state.lock().await;
        let index = Self::confirmation_index(&s, run_id, device, package)?;
        ensure!(s.items[index].confirmation.is_some(), "本人确认尚未开始");
        self.expire_confirmation(&mut s, index)?;
        Ok(Self::confirmation_response(&s, index))
    }
    pub(super) fn expire_confirmation(&self, s: &mut State, index: usize) -> Result<()> {
        if s.items[index].confirmation.as_ref().is_some_and(|c| {
            matches!(
                c.state.as_str(),
                "awaiting_scan" | "awaiting_confirmation" | "rescan_requested"
            ) && now() >= c.deadline_at
        }) {
            let mut n = s.clone();
            let confirmation = n.items[index].confirmation.as_mut().unwrap();
            confirmation.state = "timeout".into();
            confirmation.decided_at = Some(now());
            n.items[index].phase = "main".into();
            accounts::settle(&mut n, index, false, "confirmation_timeout")?;
            self.commit(s, n)?;
        }
        Ok(())
    }
    /// Reserved words are consumed before all task rules, including when early,
    /// late or sent by another viewer. They never create a billed task.
    pub(super) async fn route_control(
        &self,
        e: &LiveEvent,
        test: bool,
        persist: bool,
    ) -> Result<Option<Value>> {
        let command = if e.kind == "message" {
            e.payload["text"].as_str().unwrap_or("").trim()
        } else {
            ""
        };
        if !["确认", "取消", "扫码"].contains(&command) {
            return Ok(None);
        }
        let _notices = NoticeGuard(self);
        if !persist {
            return Ok(Some(
                json!({"reason":"control","result":"确认/取消仅在本人待确认阶段生效，预览不改变状态"}),
            ));
        }
        let mut s = self.state.lock().await;
        let mut n = s.clone();
        let event_key = e.event_id.as_ref().map(|id| {
            format!(
                "control:{}:{}:{}:{id}",
                e.platform_id, e.identity_namespace, e.room_id
            )
        });
        if event_key
            .as_ref()
            .is_some_and(|key| n.seen.contains_key(key))
        {
            return Ok(Some(json!({"reason":"duplicate","result":"重复控制消息"})));
        }
        if let Some(key) = event_key {
            n.seen.insert(key, now());
        }
        let synthetic = test || e.platform_id == "test";
        let actor = e
            .actor
            .as_ref()
            .and_then(|a| a["id"].as_str())
            .or(if synthetic { Some("test-viewer") } else { None });
        let index = n.items.iter().position(|i| {
            i.active()
                && i.state != "review"
                && !i.stop_requested
                && matches!(
                    i.phase.as_str(),
                    "awaiting_scan" | "awaiting_confirmation" | "rescanning"
                )
                && i.confirmation.as_ref().is_some_and(|c| {
                    matches!(
                        c.state.as_str(),
                        "awaiting_scan" | "awaiting_confirmation" | "rescan_requested"
                    ) && c.accepts_time(e)
                })
                && i.billing
                    .as_ref()
                    .and_then(|b| n.users.get(&b.user_id))
                    .is_some_and(|u| {
                        u.test == synthetic
                            && u.platform_id == e.platform_id
                            && u.identity_namespace == e.identity_namespace
                            && u.room_id == e.room_id
                            && Some(u.open_id.as_str()) == actor
                    })
        });
        let mut item_id = None;
        let mut result = "仅展示：当前不在本人确认阶段".to_string();
        if let Some(index) = index {
            let expired = now() >= n.items[index].confirmation.as_ref().unwrap().deadline_at;
            let stage = n.items[index].confirmation.as_ref().unwrap().state.as_str();
            if !expired
                && ((command == "确认" && stage != "awaiting_confirmation")
                    || (command == "扫码" && stage == "rescan_requested"))
            {
                n.receipts.push(Receipt {
                    seq: n.next_seq,
                    reason: "display_only".into(),
                    public_name: String::new(),
                    at: now(),
                    event: summary(e),
                    rule: None,
                    item_id: Some(n.items[index].id.clone()),
                    result: if command == "确认" {
                        "尚未扫码登录，确认无效"
                    } else {
                        "正在重新扫码，请稍候"
                    }
                    .into(),
                });
                n.next_seq += 1;
                self.commit(&mut s, n)?;
                return Ok(Some(
                    json!({"reason":"display_only","result":"当前登录阶段不接受此操作"}),
                ));
            }
            let decision = if expired {
                "timeout"
            } else if command == "确认" {
                "confirmed"
            } else if command == "扫码" {
                "rescan_requested"
            } else {
                "cancelled"
            };
            let confirmation = n.items[index].confirmation.as_mut().unwrap();
            confirmation.state = decision.into();
            confirmation.decided_at = Some(now());
            confirmation.decision_event_id = e.event_id.clone();
            n.items[index].phase = if decision == "rescan_requested" {
                "rescanning"
            } else {
                "main"
            }
            .into();
            if matches!(decision, "cancelled" | "timeout") {
                accounts::settle(&mut n, index, false, "confirmation_declined")?;
            }
            item_id = Some(n.items[index].id.clone());
            result = match decision {
                "confirmed" => "已确认，领奖成功后扣次",
                "cancelled" => "已取消，正在退出账号",
                "rescan_requested" => "正在退出登录并重新扫码，等待期限不延长",
                _ => "确认超时，正在退出账号",
            }
            .into();
        }
        n.receipts.push(Receipt {
            seq: n.next_seq,
            reason: if item_id.is_some() {
                "confirmation"
            } else {
                "display_only"
            }
            .into(),
            public_name: String::new(),
            at: now(),
            event: summary(e),
            rule: None,
            result: result.clone(),
            item_id: item_id.clone(),
        });
        n.next_seq += 1;
        self.commit(&mut s, n)?;
        Ok(Some(
            json!({"reason":"control","result":result,"item_id":item_id}),
        ))
    }
}
