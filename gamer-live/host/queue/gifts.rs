use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct GiftCredit {
    pub user_id: String,
    pub unit_price: u64,
    pub count: u64,
    pub added: u64,
    pub credited_at: i64,
}

pub(super) enum Outcome {
    Ignored,
    Duplicate,
    Added(GiftCredit),
}

/// OpenLive price is per gift: 1000 = CNY 1 = 10 batteries.
/// Round once for this message's quantity; combo_count is cumulative display data.
pub(super) fn credit(s: &mut State, e: &LiveEvent) -> Result<Outcome> {
    if e.platform_id != "bilibili" || e.kind != "gift" || e.payload["paid"] != true {
        return Ok(Outcome::Ignored);
    }
    let event_id = e
        .event_id
        .as_deref()
        .filter(|id| !id.trim().is_empty() && id.len() <= 512)
        .context("付费礼物缺少有效消息 ID，未增加次数")?;
    let key = format!(
        "gift-{:x}",
        Sha256::digest(serde_json::to_vec(&(
            &e.platform_id,
            &e.identity_namespace,
            &e.room_id,
            event_id,
        ))?)
    );
    if s.gift_credits.contains_key(&key) {
        return Ok(Outcome::Duplicate);
    }
    let unit_price = e.payload["price"]
        .as_u64()
        .filter(|price| *price > 0)
        .context("付费礼物缺少有效单价，未增加次数")?;
    let count = e.payload["count"]
        .as_u64()
        .filter(|count| *count > 0)
        .context("付费礼物缺少有效数量，未增加次数")?;
    let added = u64::try_from((u128::from(unit_price) * u128::from(count)).div_ceil(1000))
        .context("礼物折算次数超出上限，未增加次数")?;
    let (user_id, _) = accounts::account_key(e, false)?;
    let balance = s.users.get(&user_id).map_or(s.default_quota, |u| u.balance);
    let new_balance = balance
        .checked_add(added)
        .filter(|balance| *balance <= accounts::MAX_BALANCE)
        .context("礼物入账后余额超出上限，未增加次数")?;
    let user = accounts::account_for_event(s, e, false)?;
    user.balance = new_balance;
    user.changed();
    let credit = GiftCredit {
        user_id,
        unit_price,
        count,
        added,
        credited_at: now(),
    };
    // Keep this ledger with balances, independently of expiring interaction logs
    // and connection sessions. Atomic queue persistence commits both together.
    s.gift_credits.insert(key, credit.clone());
    Ok(Outcome::Added(credit))
}
