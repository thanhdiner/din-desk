use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub created: u64,
    pub(crate) session_id: String,
    pub(crate) vm_id: String,
}

impl Conversation {
    pub fn summary(&self) -> ConversationSummary {
        ConversationSummary {
            id: self.id.clone(),
            created: self.created,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ConversationSummary {
    pub id: String,
    pub created: u64,
}
