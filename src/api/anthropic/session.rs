//! Client identity is a cache namespace, never proof of transcript continuity.
use axum::http::HeaderMap;
use serde_json::Value;

fn uuid(value: &str, field: &str) -> Result<String, String> {
    uuid::Uuid::parse_str(value.trim())
        .map(|id| id.to_string())
        .map_err(|_| format!("{field} must contain a session UUID"))
}

pub fn resolve(headers: &HeaderMap, body: &Value) -> Result<Option<String>, String> {
    // A launcher ID changes on restart; Claude's conversation ID survives resume.
    let native_header = headers
        .get("x-claude-code-session-id")
        .map(|value| {
            value
                .to_str()
                .map_err(|_| "Invalid x-claude-code-session-id".to_string())
                .and_then(|value| uuid(value, "x-claude-code-session-id"))
        })
        .transpose()?;
    let user = body
        .pointer("/metadata/user_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let metadata = if user.starts_with('{') && user.len() <= 4096 {
        serde_json::from_str::<Value>(user).ok().and_then(|value| {
            value
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    } else {
        user.rsplit_once("_session_").map(|(_, id)| id.to_owned())
    };
    let metadata = metadata
        .map(|value| uuid(&value, "metadata.user_id session_id"))
        .transpose()?;
    if let (Some(header), Some(metadata)) = (&native_header, &metadata) {
        if header != metadata {
            return Err("Conflicting Claude session IDs in header and metadata".into());
        }
    }
    if let Some(id) = native_header.or(metadata) {
        return Ok(Some(format!("claude:{id}")));
    }
    headers
        .get("x-muse-client-id")
        .map(|value| {
            value
                .to_str()
                .map_err(|_| "Invalid x-muse-client-id".to_string())
                .and_then(|value| uuid(value, "x-muse-client-id"))
                .map(|id| format!("launcher:{id}"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const A: &str = "b608136c-ddb5-4ec9-b7f2-0bb23cd3e183";
    const B: &str = "bb41cd09-9383-4ee4-b30c-f909df4e5b70";
    #[test]
    fn native_session_survives_launcher_change_and_formats_agree() {
        let mut headers = HeaderMap::new();
        headers.insert("x-muse-client-id", B.parse().unwrap());
        let legacy = json!({"metadata":{"user_id":format!("user_account_x_session_{A}")}});
        let encoded = json!({"metadata":{"user_id":json!({"device_id":"device", "account_uuid":"account","session_id":A}).to_string()}});
        let expected = Some(format!("claude:{A}"));
        assert_eq!(resolve(&headers, &legacy).unwrap(), expected);
        headers.insert("x-muse-client-id", A.parse().unwrap());
        assert_eq!(resolve(&headers, &encoded).unwrap(), expected);
        headers.insert(
            "x-claude-code-session-id",
            A.to_uppercase().parse().unwrap(),
        );
        assert_eq!(resolve(&headers, &json!({})).unwrap(), expected);
        assert_eq!(resolve(&headers, &legacy).unwrap(), expected);
        headers.insert("x-claude-code-session-id", B.parse().unwrap());
        assert!(resolve(&headers, &legacy).is_err());
    }
    #[test]
    fn missing_session_uses_launcher_but_never_account_or_user_id() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            resolve(&headers, &json!({"metadata":{"user_id":"same-account"}})).unwrap(),
            None
        );
        assert_eq!(
            resolve(
                &headers,
                &json!({"metadata":{"user_id":"{\"account_uuid\":\"same-account\"}"}})
            )
            .unwrap(),
            None
        );
        headers.insert("x-muse-client-id", A.parse().unwrap());
        assert_eq!(
            resolve(&headers, &json!({})).unwrap(),
            Some(format!("launcher:{A}"))
        );
        headers.insert("x-claude-code-session-id", "not-a-uuid".parse().unwrap());
        assert!(resolve(&headers, &json!({})).is_err());
    }
}
