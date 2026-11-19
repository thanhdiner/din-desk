use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuickReplies {
    pub id: String,
    pub options: Vec<String>,
}

pub fn quick_replies(widgets: &Value) -> Vec<QuickReplies> {
    widgets
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|widget| {
            if widget.get("kind").and_then(Value::as_str) != Some("option") {
                return None;
            }
            let id = widget.get("widget_id")?.as_str()?.to_owned();
            let options: Vec<_> = widget
                .pointer("/data/options")?
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .filter(|text| !text.trim().is_empty())
                .map(str::to_owned)
                .collect();
            if id.is_empty() || options.is_empty() {
                return None;
            }
            Some(QuickReplies { id, options })
        })
        .collect()
}

/// Holds only a possible marker prefix so markers split across SSE deltas do
/// not leak to text clients. Ordinary brackets and Markdown pass through.
#[derive(Default)]
pub struct WidgetTextFilter {
    pending: String,
}

impl WidgetTextFilter {
    pub fn push(&mut self, text: &str) -> String {
        self.pending.push_str(text);
        let mut output = String::new();
        loop {
            let Some(start) = self.pending.find('[') else {
                output.push_str(&std::mem::take(&mut self.pending));
                break;
            };
            output.push_str(&self.pending[..start]);
            self.pending.drain(..start);
            let prefix = ["[[hatch_widget:", "[hatch_widget:"]
                .into_iter()
                .find(|prefix| self.pending.starts_with(prefix));
            if let Some(prefix) = prefix {
                let close = if prefix.starts_with("[[") { "]]" } else { "]" };
                if let Some(end) = self.pending[prefix.len()..].find(close) {
                    let end = prefix.len() + end;
                    let id = &self.pending[prefix.len()..end];
                    if !id.is_empty()
                        && id.len() <= 256
                        && !id.chars().any(|c| c.is_whitespace() || c == ']')
                    {
                        self.pending.drain(..end + close.len());
                        continue;
                    }
                } else if self.pending.len() <= prefix.len() + 256 + close.len()
                    && !self.pending[prefix.len()..]
                        .chars()
                        .any(char::is_whitespace)
                {
                    break;
                }
            } else if ["[[hatch_widget:", "[hatch_widget:"]
                .iter()
                .any(|prefix| prefix.starts_with(self.pending.as_str()))
            {
                break;
            }
            // This '[' is ordinary text, rather than a Muse marker.
            output.push('[');
            self.pending.remove(0);
        }
        output
    }

    pub fn finish(&mut self) -> String {
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removes_markers_at_every_character_boundary_without_eating_markdown() {
        let input = "Chọn [mục](url): [[hatch_widget:widget-a]] hoặc [hatch_widget:widget-b].";
        for split in input
            .char_indices()
            .map(|(index, _)| index)
            .chain([input.len()])
        {
            let mut filter = WidgetTextFilter::default();
            let result =
                filter.push(&input[..split]) + &filter.push(&input[split..]) + &filter.finish();
            assert_eq!(result, "Chọn [mục](url):  hoặc .", "split={split}");
        }
    }
    #[test]
    fn preserves_incomplete_or_invalid_markup() {
        for text in [
            "[",
            "[[not a widget]]",
            "[hatch_widget:unfinished",
            "[hatch_widget:bad id]",
            "[hatch_widget:]",
        ] {
            let mut filter = WidgetTextFilter::default();
            assert_eq!(filter.push(text) + &filter.finish(), text);
        }
    }
}
