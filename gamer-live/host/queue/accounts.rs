use super::*;
use sha2::{Digest, Sha256};

const MAX_BALANCE: u64 = 1_000_000_000;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub platform_id: String,
    pub identity_namespace: String,
    pub room_id: String,
    pub open_id: String,
    pub name: String,
    pub balance: u64,
    pub reserved: u64,
    pub unlimited: bool,
    pub version: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub test: bool,
    #[serde(default)]
    last_update: Option<AdminChange>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct AdminChange {
    request_id: String,
    balance: u64,
    unlimited: bool,
}
impl Account {
    pub(super) fn available(&self) -> u64 {
        self.balance.saturating_sub(self.reserved)
    }
    pub(super) fn row(&self) -> Value {
        json!({"id":self.id,"platform_id":self.platform_id,"identity_namespace":self.identity_namespace,
            "room_id":self.room_id,"open_id":self.open_id,"name":self.name,"balance":self.balance,
            "reserved":self.reserved,"available":if self.unlimited {Value::Null} else {json!(self.available())},
            "unlimited":self.unlimited,"version":self.version,"created_at":self.created_at,
            "updated_at":self.updated_at,"test":self.test})
    }
    fn changed(&mut self) {
        self.version += 1;
        self.updated_at = now();
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Billing {
    pub user_id: String,
    pub cost: u64,
    pub state: String,
    pub chargeable: bool,
    pub created_at: i64,
    pub settled_at: Option<i64>,
    pub reason: Option<String>,
}
pub(super) fn account_key(e: &LiveEvent, test: bool) -> Result<(String, String)> {
    let synthetic = test || e.platform_id == "test";
    let open_id = e
        .actor
        .as_ref()
        .and_then(|a| a["id"].as_str())
        .filter(|id| !id.is_empty())
        .or(if synthetic { Some("test-viewer") } else { None })
        .context("平台未提供 open_id，不能入队")?;
    ensure!(open_id.len() <= 512, "open_id 过长");
    ensure!(
        synthetic || (!e.identity_namespace.is_empty() && !e.room_id.is_empty()),
        "平台应用或直播间身份缺失，不能入队"
    );
    let key = serde_json::to_vec(&(
        synthetic,
        &e.platform_id,
        &e.identity_namespace,
        &e.room_id,
        open_id,
    ))?;
    Ok((format!("user-{:x}", Sha256::digest(key)), open_id.into()))
}
pub(super) fn reserve(s: &mut State, e: &LiveEvent, test: bool, cost: u64) -> Result<Billing> {
    ensure!(
        (1..=MAX_BALANCE).contains(&cost),
        "扣除次数须为正整数且不超过上限"
    );
    let (id, open_id) = account_key(e, test)?;
    let synthetic = test || e.platform_id == "test";
    let name = e
        .actor
        .as_ref()
        .and_then(|a| a["name"].as_str())
        .unwrap_or("观众")
        .chars()
        .take(160)
        .collect::<String>();
    let starting_balance = s.default_quota;
    let user = s.users.entry(id.clone()).or_insert_with(|| Account {
        id: id.clone(),
        platform_id: e.platform_id.clone(),
        identity_namespace: e.identity_namespace.clone(),
        room_id: e.room_id.clone(),
        open_id,
        name: name.clone(),
        balance: starting_balance,
        reserved: 0,
        unlimited: synthetic,
        version: 0,
        created_at: now(),
        updated_at: now(),
        test: synthetic,
        last_update: None,
    });
    user.name = name;
    ensure!(
        user.unlimited || user.available() >= cost,
        "可用次数不足（可用{}，本次需要{}）",
        user.available(),
        cost
    );
    if !user.unlimited {
        user.reserved = user.reserved.checked_add(cost).context("预占次数溢出")?;
    }
    user.changed();
    Ok(Billing {
        user_id: id,
        cost,
        state: if user.unlimited { "exempt" } else { "reserved" }.into(),
        chargeable: !user.unlimited,
        created_at: now(),
        settled_at: None,
        reason: None,
    })
}
pub(super) fn reserve_existing(s: &mut State, user_id: &str, cost: u64) -> Result<Billing> {
    let user = s
        .users
        .get_mut(user_id)
        .context("旧任务没有可用计费账户，请重新发送弹幕")?;
    ensure!(
        user.unlimited || user.available() >= cost,
        "可用次数不足，不能重新入队"
    );
    if !user.unlimited {
        user.reserved = user.reserved.checked_add(cost).context("预占次数溢出")?;
    }
    user.changed();
    Ok(Billing {
        user_id: user_id.into(),
        cost,
        state: if user.unlimited { "exempt" } else { "reserved" }.into(),
        chargeable: !user.unlimited,
        created_at: now(),
        settled_at: None,
        reason: None,
    })
}
/// Balance and reservation transition in the very same queue snapshot.
pub(super) fn settle(s: &mut State, index: usize, success: bool, reason: &str) -> Result<()> {
    let Some(billing) = s.items[index].billing.as_ref() else {
        return Ok(());
    };
    if billing.state != "reserved" {
        return Ok(());
    }
    let cost = billing.cost;
    let user = s
        .users
        .get_mut(&billing.user_id)
        .context("次数账户缺失，需核对")?;
    ensure!(
        user.reserved >= cost && (!success || user.balance >= cost),
        "次数账户与预占不一致，需核对"
    );
    user.reserved -= cost;
    if success {
        user.balance -= cost;
    }
    user.changed();
    let billing = s.items[index].billing.as_mut().unwrap();
    billing.state = if success { "charged" } else { "released" }.into();
    billing.settled_at = Some(now());
    billing.reason = Some(reason.into());
    Ok(())
}
impl Queue {
    pub(super) fn notice_rejected(s: &State, receipt: &Receipt) -> Value {
        let event = &receipt.event;
        let key = json!([
            event["platform_id"],
            event["identity_namespace"],
            event["room_id"],
            event["event_id"],
            event["kind"],
            event["actor"]["id"],
            event["received_at"],
            event["payload"]
        ]);
        let id = format!("rejected:{:x}", Sha256::digest(key.to_string().as_bytes()));
        let test = event["test"] == true || event["platform_id"] == "test";
        let user = s.users.values().find(|user| {
            user.test == test
                && Some(user.platform_id.as_str()) == event["platform_id"].as_str()
                && Some(user.identity_namespace.as_str()) == event["identity_namespace"].as_str()
                && Some(user.room_id.as_str()) == event["room_id"].as_str()
                && Some(user.open_id.as_str()) == event["actor"]["id"].as_str()
        });
        json!({"kind":"rejected","id":id,"item_id":Value::Null,"number":receipt.seq,
            "viewer":event["actor"]["name"],"open_id":event["actor"]["id"],
            "platform_id":event["platform_id"],"identity_namespace":event["identity_namespace"],"room_id":event["room_id"],
            "test":test,"reason":"quota_unavailable",
            "remaining":user.map(|u|u.balance),"reserved":user.map(|u|u.reserved),"available":user.map(|u|u.available()),
            "unlimited":user.is_some_and(|u|u.unlimited),"text":"本次未入队：可用次数不足或平台未提供完整观众身份"})
    }
    pub async fn accounts_list(&self, offset: usize, search: &str) -> Value {
        let s = self.state.lock().await;
        let search = search.to_lowercase();
        let mut users: Vec<_> = s
            .users
            .values()
            .filter(|u| {
                search.is_empty()
                    || u.name.to_lowercase().contains(&search)
                    || u.open_id.to_lowercase().contains(&search)
                    || u.id.contains(&search)
            })
            .collect();
        users.sort_by_key(|u| (std::cmp::Reverse(u.updated_at), u.id.as_str()));
        let offset = offset.min(users.len());
        json!({"users":users.iter().skip(offset).take(50).map(|u|u.row()).collect::<Vec<_>>(),
            "has_more":users.len()>offset+50,"next_offset":(offset+50).min(users.len()),"total":users.len(),
            "settings":{"default_quota":s.default_quota,"version":s.account_settings_version}})
    }
    pub async fn accounts_settings(&self) -> Value {
        let s = self.state.lock().await;
        json!({"default_quota":s.default_quota,"version":s.account_settings_version})
    }
    pub async fn accounts_save_settings(
        &self,
        default_quota: u64,
        expected_version: u64,
    ) -> Result<Value> {
        ensure!(default_quota <= MAX_BALANCE, "默认次数超出上限");
        let mut s = self.state.lock().await;
        ensure!(
            s.account_settings_version == expected_version,
            "默认次数已被修改，请重新读取"
        );
        let mut n = s.clone();
        n.default_quota = default_quota;
        n.account_settings_version += 1;
        self.commit(&mut s, n)?;
        Ok(json!({"default_quota":s.default_quota,"version":s.account_settings_version}))
    }
    pub async fn accounts_update(
        &self,
        user_id: &str,
        balance: u64,
        unlimited: bool,
        expected_version: u64,
        request_id: &str,
    ) -> Result<Value> {
        ensure!(
            balance <= MAX_BALANCE && !request_id.is_empty() && request_id.len() <= 128,
            "次数或请求 ID 无效"
        );
        let mut s = self.state.lock().await;
        let user = s.users.get(user_id).context("用户不存在")?;
        if let Some(previous) = &user.last_update {
            if previous.request_id == request_id {
                ensure!(
                    previous.balance == balance && previous.unlimited == unlimited,
                    "同一请求 ID 不能更改操作内容"
                );
                return Ok(user.row());
            }
        }
        ensure!(
            user.version == expected_version,
            "用户次数已被修改，请重新读取"
        );
        ensure!(
            balance >= user.reserved,
            "余额不能低于已预占次数{}",
            user.reserved
        );
        let mut n = s.clone();
        let user = n.users.get_mut(user_id).unwrap();
        user.balance = balance;
        user.unlimited = unlimited;
        user.last_update = Some(AdminChange {
            request_id: request_id.into(),
            balance,
            unlimited,
        });
        user.changed();
        let response = user.row();
        self.commit(&mut s, n)?;
        Ok(response)
    }
    pub(super) fn notice_item(s: &State, item: &Item, kind: &str) -> Value {
        let user = item.billing.as_ref().and_then(|b| s.users.get(&b.user_id));
        let message = &item.event["message"];
        let cost = item.billing.as_ref().map_or(0, |b| b.cost);
        let position = if item.pending() {
            s.items
                .iter()
                .filter(|i| i.pending())
                .position(|i| i.id == item.id)
                .map_or(0, |i| i + 1)
        } else {
            0
        };
        let text = match kind {
            "queued" => format!(
                "{}，已加入队列 #{}，本次预占{}次",
                message["actor"]["name"].as_str().unwrap_or("观众"),
                item.number,
                cost
            ),
            "confirmation_required" => format!(
                "#{} 登录已就绪，请本人在5分钟内发送“确认”或“取消”",
                item.number
            ),
            "confirmed" => format!(
                "#{} 已确认，开始执行；领奖成功后扣除{}次",
                item.number, cost
            ),
            "cancelled" | "timeout" => {
                format!("#{} 未确认执行，退出登录并释放预占次数", item.number)
            }
            "charged" => format!("#{} 领奖成功，已扣除{}次", item.number, cost),
            "released" => format!("#{} 未完成，预占次数已释放", item.number),
            "succeeded" => format!("#{} 已完成", item.number),
            "failed" => format!("#{} 未完成", item.number),
            "cleanup_warning" => format!("#{} 收尾异常，请主播检查目标状态", item.number),
            _ => String::new(),
        };
        json!({"kind":kind,"id":format!("{}:{kind}",item.id),"item_id":item.id,"number":item.number,
            "viewer":message["actor"]["name"],"open_id":user.map(|u|u.open_id.as_str()),
            "platform_id":user.map(|u|u.platform_id.as_str()),"identity_namespace":user.map(|u|u.identity_namespace.as_str()),
            "room_id":user.map(|u|u.room_id.as_str()),"test":item.event["test"],"cost":cost,
            "remaining":user.map(|u|u.balance),"reserved":user.map(|u|u.reserved),
            "available":user.filter(|u|!u.unlimited).map(|u|u.available()),"unlimited":user.is_some_and(|u|u.unlimited),
            "deadline_at":item.confirmation.as_ref().map(|c|c.deadline_at),"position":position,"text":text})
    }
}
