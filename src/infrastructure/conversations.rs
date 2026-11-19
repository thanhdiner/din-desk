use crate::{core::conversation::Conversation, ports::ConversationStore};
use std::{fs, io, path::PathBuf, sync::Mutex};

/// One server process owns this file. Move to a shared store before scaling out.
pub struct FileConversationStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileConversationStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }
    fn read(&self) -> io::Result<Vec<Conversation>> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
}

impl ConversationStore for FileConversationStore {
    fn insert(&self, conversation: Conversation) -> io::Result<()> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("conversation store lock poisoned"))?;
        let mut rows = self.read()?;
        if rows.iter().any(|row| row.id == conversation.id) {
            return Err(io::Error::other("duplicate conversation ID"));
        }
        rows.push(conversation);
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            use std::io::Write;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec_pretty(&rows).map_err(io::Error::other)?)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    fn get(&self, id: &str) -> io::Result<Option<Conversation>> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("conversation store lock poisoned"))?;
        Ok(self.read()?.into_iter().find(|row| row.id == id))
    }
    fn list(&self) -> io::Result<Vec<Conversation>> {
        let _lock = self
            .lock
            .lock()
            .map_err(|_| io::Error::other("conversation store lock poisoned"))?;
        self.read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mappings_survive_reopening_and_insertion_preserves_other_rows() {
        let directory =
            std::env::temp_dir().join(format!("muse-conversations-{}", uuid::Uuid::new_v4()));
        let path = directory.join("conversations.json");
        let store = FileConversationStore::new(path.clone());
        for id in ["a", "b"] {
            store
                .insert(Conversation {
                    id: id.into(),
                    created: 1,
                    session_id: format!("session-{id}"),
                    vm_id: "vm".into(),
                })
                .unwrap();
        }
        let reopened = FileConversationStore::new(path);
        assert_eq!(reopened.get("a").unwrap().unwrap().session_id, "session-a");
        assert_eq!(reopened.list().unwrap().len(), 2);
        assert!(reopened.get("missing").unwrap().is_none());
        fs::remove_file(directory.join("conversations.json")).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
