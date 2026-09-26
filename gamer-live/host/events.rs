use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};

#[derive(Clone, Serialize, serde::Deserialize)]
pub struct LiveEvent {
    pub schema_version: u8,
    pub seq: u64,
    pub platform_id: String,
    pub connection_id: String,
    pub room_id: String,
    pub event_id: Option<String>,
    pub kind: String,
    pub occurred_at: Option<i64>,
    pub received_at: String,
    pub actor: Option<Value>,
    pub payload: Value,
    pub platform_data: Value,
}
pub struct EventBuffer {
    next: u64,
    events: VecDeque<LiveEvent>,
    ids: HashSet<String>,
    order: VecDeque<String>,
}
impl Default for EventBuffer {
    fn default() -> Self {
        Self {
            next: 1,
            events: VecDeque::new(),
            ids: HashSet::new(),
            order: VecDeque::new(),
        }
    }
}
fn scalar(value: Option<&Value>) -> String {
    value
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        })
        .unwrap_or_default()
}
impl EventBuffer {
    pub fn push(&mut self, connection: &str, raw: Value) -> Option<LiveEvent> {
        // Bound retained raw payloads as well as event count.
        if raw.to_string().len() > 64 * 1024 {
            return None;
        }
        let cmd = raw["cmd"].as_str().unwrap_or("unknown");
        let kind = match cmd
            .strip_prefix("OPEN_LIVEROOM_")
            .or_else(|| cmd.strip_prefix("LIVE_OPEN_PLATFORM_"))
            .or_else(|| cmd.strip_prefix("OPEN_PLATFORM_"))
            .unwrap_or(cmd)
        {
            "DM" => "message",
            "DM_MIRROR" => "message.mirror",
            "SEND_GIFT" => "gift",
            "SUPER_CHAT" => "super_chat",
            "SUPER_CHAT_DEL" => "message.removed",
            "GUARD" => "membership",
            "LIKE" => "like",
            "LIVE_ROOM_ENTER" | "ENTER_ROOM" => "enter",
            "INTERACT_WORD" => "follow",
            "LIVE_START" => "room.started",
            "LIVE_END" => "room.ended",
            "ROOM_CHANGE" => "room.changed",
            "ROOM_BLOCK_MSG" => "user.blocked",
            "WARNING" => "room.warning",
            "INTERACTION_END" => "connection.ended",
            _ => "platform.event",
        }
        .to_owned();
        let data = &raw["data"];
        let room = scalar(data.get("room_id"));
        let id = data
            .get("msg_id")
            .map(|v| scalar(Some(v)))
            .filter(|v| !v.is_empty());
        if let Some(id) = &id {
            let key = format!("{connection}:{room}:{cmd}:{id}");
            if !self.ids.insert(key.clone()) {
                return None;
            }
            self.order.push_back(key);
            while self.order.len() > 2048 {
                if let Some(old) = self.order.pop_front() {
                    self.ids.remove(&old);
                }
            }
        }
        let actor = data
            .get("open_id")
            .filter(|v| v.is_string())
            .map(|id| json!({"id":id,"name":data["uname"],"avatar":data["uface"]}));
        let event = LiveEvent {
            schema_version: 1,
            seq: self.next,
            platform_id: "bilibili".into(),
            connection_id: connection.into(),
            room_id: room,
            event_id: id,
            kind,
            occurred_at: data["timestamp"].as_i64(),
            received_at: chrono::Utc::now().to_rfc3339(),
            actor,
            payload: json!({"text":data.get("msg").or_else(||data.get("message")),"gift_id":data["gift_id"],"gift_name":data["gift_name"],"count":data.get("gift_num").or_else(||data.get("like_count")),"paid":data["paid"]}),
            platform_data: raw,
        };
        self.next += 1;
        self.events.push_back(event.clone());
        while self.events.len() > 500 {
            self.events.pop_front();
        }
        Some(event)
    }
    pub fn page(&self, after: u64) -> Value {
        let oldest = self.events.front().map_or(self.next, |e| e.seq);
        let events: Vec<_> = self
            .events
            .iter()
            .filter(|e| e.seq > after)
            .take(100)
            .collect();
        let cursor = events.last().map_or(after, |e| e.seq);
        json!({"events":events,"next_seq":cursor,"oldest_seq":oldest,"gap":after>0&&after.saturating_add(1)<oldest})
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scopes_dedup_and_reports_gaps() {
        let mut b = EventBuffer::default();
        let e = json!({"cmd":"OPEN_LIVEROOM_DM","data":{"room_id":1,"msg_id":"a","msg":"hello"}});
        b.push("a", e.clone());
        b.push("a", e.clone());
        b.push("b", e);
        assert_eq!(b.page(0)["events"].as_array().unwrap().len(), 2);
        for n in 0..600 {
            b.push(
                "b",
                json!({"cmd":"OPEN_LIVEROOM_LIKE","data":{"msg_id":n.to_string()}}),
            );
        }
        assert_eq!(b.page(1)["gap"], true);
        assert_eq!(b.page(1)["events"].as_array().unwrap().len(), 100);
    }
    #[test]
    fn preserves_missing_identity_and_platform_fields() {
        let mut b = EventBuffer::default();
        b.push(
            "c",
            json!({"cmd":"OPEN_LIVEROOM_DM_MIRROR","data":{"msg":"x","extra":123}}),
        );
        let p = b.page(0);
        assert!(p["events"][0]["actor"].is_null());
        assert_eq!(p["events"][0]["platform_data"]["data"]["extra"], 123);
    }
}
