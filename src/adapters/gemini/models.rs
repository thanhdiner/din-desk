//! Model IDs and thinking enums come from this account's GetUserStatus response,
//! including the current mode policy (field 25), not a global hardcoded catalog.
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub account_index: u8,
    pub model_id: String,
    pub thinking_id: String,
}
impl Options {
    pub fn from_env() -> Result<Self, String> {
        let index = std::env::var("GEMINI_ACCOUNT_INDEX").unwrap_or_else(|_| "0".into());
        let options = Self {
            account_index: index.parse().map_err(|_| "Invalid GEMINI_ACCOUNT_INDEX")?,
            model_id: std::env::var("GEMINI_MODEL_ID").unwrap_or_default(),
            thinking_id: std::env::var("GEMINI_THINKING_ID").unwrap_or_default(),
        };
        options.validate()?;
        Ok(options)
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.account_index > 99
            || self.model_id.len() > 128
            || self.thinking_id.len() > 160
            || self
                .model_id
                .chars()
                .chain(self.thinking_id.chars())
                .any(|c| !c.is_ascii_alphanumeric() && !"-_:".contains(c))
            || (self.model_id.is_empty() && !self.thinking_id.is_empty())
        {
            return Err("Invalid Gemini model, thinking level or Google account index".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ThinkingLevel {
    pub id: String,
    pub name: String,
    pub level: u64,
    pub routing_id: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub description: String,
    pub available: bool,
    pub category: u64,
    pub levels: Vec<ThinkingLevel>,
    #[serde(skip)]
    capacity: u64,
    #[serde(skip)]
    capacity_index: usize,
    #[serde(skip)]
    mode_route: Option<String>,
    #[serde(skip)]
    policy: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Catalog {
    pub account_index: u8,
    pub models: Vec<Model>,
}
#[derive(Clone, Debug)]
pub struct Selection {
    model: Model,
    thinking: Option<ThinkingLevel>,
}
impl Catalog {
    pub fn select(&self, options: &Options) -> Result<Selection, String> {
        let model = self.models.iter().find(|m| m.id == options.model_id && m.available)
            .ok_or("Gemini model is locked or no longer available for this Google account; reload model choices")?;
        let thinking = if options.thinking_id.is_empty() {
            None
        } else {
            Some(model.levels.iter().find(|l| l.id == options.thinking_id)
                .ok_or("Gemini thinking level is not available for the selected model; reload model choices")?.clone())
        };
        Ok(Selection {
            model: model.clone(),
            thinking,
        })
    }
}
impl Selection {
    pub fn header(&self, instance_id: &str, build: &str) -> String {
        let mut fields = vec![Value::Null; 26];
        fields[0] = json!(1);
        if !self.model.policy {
            fields[4] = json!(self.model.id);
        }
        if let Some(route) = &self.model.mode_route {
            fields[24] = json!(route);
        }
        if let Some(route) = self.thinking.as_ref().and_then(|t| t.routing_id.as_ref()) {
            fields[25] = json!(route);
        }
        fields[6] = json!("en");
        fields[7] = json!(0);
        fields[8] = json!([4, 33, 22, 5, 6, 8, 16, 19, 20, 38]);
        if !self.model.policy {
            fields[self.model.capacity_index] = json!(self.model.capacity);
        }
        fields[14] = json!(self.model.category);
        fields[15] = json!(self.thinking.as_ref().map_or(0, |t| t.level));
        fields[16] = json!(instance_id);
        fields[17] = json!([build]);
        json!(fields).to_string()
    }
    pub fn apply_body(&self, body: &mut Value) -> Result<(), String> {
        let mut inner: Vec<Value> =
            serde_json::from_str(body[1].as_str().ok_or("Invalid Gemini request body")?)
                .map_err(|_| "Invalid Gemini request body")?;
        inner.resize(81, Value::Null);
        inner[79] = json!(self.model.category);
        if let Some(thinking) = &self.thinking {
            inner[80] = json!(thinking.level);
        }
        body[1] = json!(serde_json::to_string(&inner).map_err(|_| "Invalid Gemini request body")?);
        Ok(())
    }
}

fn text(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= 500 && !s.chars().any(char::is_control))
        .map(str::to_owned)
}
fn identifier(v: &Value) -> Option<String> {
    text(v).filter(|s| {
        s.len() <= 128
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
    })
}
fn contains(values: &Value, needle: u64) -> bool {
    values
        .as_array()
        .is_some_and(|items| items.iter().any(|v| v.as_u64() == Some(needle)))
}
pub fn parse(payload: &Value, account_index: u8) -> Result<Catalog, String> {
    if payload[14].as_u64().is_some_and(|n| n != 1000 && n != 1040) {
        return Err(
            "Gemini Google account is not available; verify the account index and cookie".into(),
        );
    }
    let raw = payload[15]
        .as_array()
        .ok_or("Gemini model catalog is missing")?;
    if raw.is_empty() || raw.len() > 64 {
        return Err("Invalid Gemini model catalog size".into());
    }
    let (capacity, capacity_index) = if contains(&payload[16], 21) {
        (1, 12)
    } else if contains(&payload[16], 22) {
        (2, 12)
    } else if contains(&payload[17], 115) {
        (4, 11)
    } else if contains(&payload[16], 16) || contains(&payload[17], 106) {
        (3, 11)
    } else if contains(&payload[16], 8) || contains(&payload[17], 19) {
        (2, 11)
    } else {
        (1, 11)
    };
    let mut legacy = Vec::new();
    for item in raw {
        let id = identifier(&item[0]).ok_or("Invalid Gemini model identity")?;
        let category = item[17]
            .as_u64()
            .or_else(|| item[9].as_u64())
            .ok_or("Missing Gemini mode category")?;
        let name = text(&item[2])
            .or_else(|| text(&item[10]))
            .or_else(|| text(&item[1]))
            .unwrap_or_else(|| id.clone());
        legacy.push(Model {
            id,
            name,
            description: text(&item[11])
                .or_else(|| text(&item[12]))
                .unwrap_or_default(),
            category,
            available: true,
            levels: Vec::new(),
            capacity,
            capacity_index,
            mode_route: None,
            policy: false,
        });
    }
    let mut models = Vec::new();
    if !payload[24].is_null() {
        let modes = payload[24][1]
            .as_array()
            .ok_or("Gemini model policy changed")?;
        if modes.is_empty() || modes.len() > 64 {
            return Err("Invalid Gemini model policy".into());
        }
        let mut represented = BTreeSet::new();
        for mode in modes {
            if !contains(&mode[3], 1) {
                continue;
            } // Normal chat only; exclude other feature modes.
            let info = &mode[0];
            let category = info[1].as_u64().ok_or("Missing Gemini policy category")?;
            let variant = identifier(&info[3]);
            let base = legacy.iter().find(|m| m.category == category);
            let id = variant
                .clone()
                .or_else(|| identifier(&info[0]))
                .or_else(|| base.map(|m| m.id.clone()))
                .ok_or("Gemini policy model has no selector")?;
            let locked = !info[14].is_null(); // Native upsell/lock metadata.
            let mut levels = Vec::new();
            for level in mode[1].as_array().ok_or("Missing Gemini thinking levels")? {
                let number = level[0]
                    .as_u64()
                    .filter(|n| [1, 2, 3, 4].contains(n))
                    .ok_or("Unknown Gemini thinking level")?;
                let routing_id = identifier(&level[13]);
                let key = format!(
                    "{number}{}",
                    routing_id
                        .as_ref()
                        .map(|r| format!(":{r}"))
                        .unwrap_or_default()
                );
                let name = text(&level[1]).ok_or("Missing thinking level label")?;
                levels.push(ThinkingLevel {
                    id: key,
                    name,
                    level: number,
                    routing_id,
                });
            }
            if variant.is_none() {
                if let Some(base) = base {
                    represented.insert(base.id.clone());
                }
            }
            models.push(Model {
                id,
                name: text(&info[5])
                    .or_else(|| text(&info[4]))
                    .or_else(|| base.map(|m| m.name.clone()))
                    .ok_or("Missing model label")?,
                description: text(&info[12]).unwrap_or_default(),
                category,
                available: !locked,
                levels,
                capacity,
                capacity_index,
                mode_route: variant,
                policy: true,
            });
        }
        // A legacy backend hash replaced by an explicit policy route must not
        // bypass the policy (for example Carbon replaced by allowed Argon).
        for mut model in legacy {
            if !represented.contains(&model.id) {
                model.available = false;
                models.push(model);
            }
        }
    } else {
        for mut model in legacy {
            model.levels = vec![
                ThinkingLevel {
                    id: "1".into(),
                    name: "Standard".into(),
                    level: 1,
                    routing_id: None,
                },
                ThinkingLevel {
                    id: "2".into(),
                    name: "Extended".into(),
                    level: 2,
                    routing_id: None,
                },
            ];
            models.push(model);
        }
    }
    let mut ids = BTreeSet::new();
    if models.is_empty() || models.iter().any(|m| !ids.insert(m.id.clone())) {
        return Err("Ambiguous Gemini model catalog".into());
    }
    Ok(Catalog {
        account_index,
        models,
    })
}

pub fn parse_rpc(body: &[u8], account_index: u8) -> Result<Catalog, String> {
    let mut frames = super::protocol::Frames::default();
    let mut result = None;
    for frame in frames.push(body)? {
        for part in frame.as_array().ok_or("Invalid Gemini model RPC")? {
            if part[0] == "wrb.fr" && part[1] == "otAQ7b" {
                if result.is_some() {
                    return Err("Ambiguous Gemini model RPC".into());
                }
                let payload: Value =
                    serde_json::from_str(part[2].as_str().ok_or("Invalid model RPC result")?)
                        .map_err(|_| "Invalid Gemini model RPC JSON")?;
                result = Some(parse(&payload, account_index)?);
            }
        }
    }
    frames.finish()?;
    result.ok_or("Gemini model catalog is unavailable; no model fallback performed".into())
}
