//! Remove redundant formatting, never truncate behavioral rules or schemas.
use serde_json::Value;

pub fn compact(definition: &Value) -> Value {
    let mut definition = definition.clone();
    let Some(text) = definition.get("description").and_then(Value::as_str) else {
        return definition;
    };
    let mut lines = Vec::new();
    let mut code = false;
    let mut previous = "";
    for line in text.lines() {
        let trimmed = line.trim_end();
        let fence =
            trimmed.trim_start().starts_with("```") || trimmed.trim_start().starts_with("~~~");
        if !code
            && !fence
            && !line.starts_with("    ")
            && !line.starts_with('\t')
            && trimmed == previous
            && !lines.is_empty()
        {
            continue;
        }
        lines.push(if code { line } else { trimmed });
        previous = trimmed;
        if fence {
            code = !code;
        }
    }
    definition["description"] = Value::String(lines.join("\n"));
    definition
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn preserves_rules_schema_and_code_examples_without_length_truncation() {
        let definition = json!({"name":"custom","description":"Rule MUST stay.  \nRule MUST stay.\n\n\n```text\nx\nx\n```\nNEVER remove this trailing rule.","input_schema":{"type":"object","description":"schema  whitespace\n\n","properties":{"content":{"type":"string"}},"required":["content"]}});
        let result = compact(&definition);
        assert_eq!(result["input_schema"], definition["input_schema"]);
        assert!(result["description"]
            .as_str()
            .unwrap()
            .ends_with("NEVER remove this trailing rule."));
        assert!(result["description"].as_str().unwrap().contains("x\nx"));
        assert_eq!(
            result["description"]
                .as_str()
                .unwrap()
                .matches("Rule MUST stay.")
                .count(),
            1
        );
    }
}
