//! Chat text components.
//!
//! A component is a tree: a piece of text with formatting plus child
//! components (`extra`) that inherit the formatting. The game accepts them as
//! JSON in commands/files and as NBT over the network; we build a JSON value
//! and convert it, which keeps one source of truth.

use crate::nbt::NbtTag;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Text {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub translate: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub with: Vec<Text>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bold: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub italic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub underlined: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strikethrough: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obfuscated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_event: Option<ClickEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hover_event: Option<HoverEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<Text>,
}

/// What happens when the player clicks the text. Uses the 1.21.5+ layout
/// where each action has its own field name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ClickEvent {
    OpenUrl { url: String },
    RunCommand { command: String },
    SuggestCommand { command: String },
    CopyToClipboard { value: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HoverEvent {
    ShowText { value: Box<Text> },
}

impl Text {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    pub fn empty() -> Self {
        Self::default()
    }

    /// A translatable message, resolved by the client in its own language.
    pub fn translate(key: impl Into<String>, with: Vec<Text>) -> Self {
        Self {
            translate: Some(key.into()),
            with,
            ..Default::default()
        }
    }

    /// Parses `&`-style legacy colour codes (`&a`, `&l`, `&#rrggbb`...) into a
    /// component tree. Handy for config files and mods.
    pub fn legacy(input: &str) -> Self {
        let mut parts: Vec<Text> = Vec::new();
        let mut current = Text::empty();
        let mut chars = input.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '&' && ch != '§' {
                current.text.push(ch);
                continue;
            }
            let Some(code) = chars.next() else {
                current.text.push(ch);
                break;
            };
            // Work out the style of the next segment before we close this one.
            let mut next = Text {
                color: current.color.clone(),
                bold: current.bold,
                italic: current.italic,
                underlined: current.underlined,
                strikethrough: current.strikethrough,
                obfuscated: current.obfuscated,
                ..Default::default()
            };
            match code {
                '#' => {
                    let hex: String = chars.by_ref().take(6).collect();
                    next = Text::empty();
                    next.color = Some(format!("#{hex}"));
                }
                'l' => next.bold = Some(true),
                'o' => next.italic = Some(true),
                'n' => next.underlined = Some(true),
                'm' => next.strikethrough = Some(true),
                'k' => next.obfuscated = Some(true),
                'r' => next = Text::empty(),
                c => match legacy_color(c) {
                    Some(color) => {
                        next = Text::empty();
                        next.color = Some(color.to_owned());
                    }
                    None => {
                        // Not a code: keep the characters as literal text.
                        current.text.push(ch);
                        current.text.push(c);
                        continue;
                    }
                },
            }
            if !current.text.is_empty() {
                parts.push(std::mem::replace(&mut current, next));
            } else {
                current = next;
            }
        }
        if !current.text.is_empty() {
            parts.push(current);
        }
        match parts.len() {
            0 => Text::empty(),
            1 => parts.remove(0),
            _ => Text {
                extra: parts,
                ..Default::default()
            },
        }
    }

    pub fn color(mut self, color: &str) -> Self {
        self.color = Some(color.to_owned());
        self
    }

    pub fn bold(mut self) -> Self {
        self.bold = Some(true);
        self
    }

    pub fn italic(mut self) -> Self {
        self.italic = Some(true);
        self
    }

    pub fn append(mut self, child: Text) -> Self {
        self.extra.push(child);
        self
    }

    pub fn on_click(mut self, event: ClickEvent) -> Self {
        self.click_event = Some(event);
        self
    }

    pub fn on_hover_text(mut self, text: Text) -> Self {
        self.hover_event = Some(HoverEvent::ShowText { value: Box::new(text) });
        self
    }

    /// Plain string with formatting stripped. Used for logs and the console.
    pub fn to_plain(&self) -> String {
        let mut out = String::new();
        self.collect_plain(&mut out);
        out
    }

    fn collect_plain(&self, out: &mut String) {
        if let Some(key) = &self.translate {
            // We do not ship language files; show the key with its arguments.
            out.push_str(key);
            if !self.with.is_empty() {
                out.push('(');
                for (i, arg) in self.with.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    arg.collect_plain(out);
                }
                out.push(')');
            }
        } else {
            out.push_str(&self.text);
        }
        for child in &self.extra {
            child.collect_plain(out);
        }
    }

    pub fn to_json(&self) -> Value {
        // A bare string is the compact form the game understands.
        let is_plain = self.translate.is_none()
            && self.color.is_none()
            && self.bold.is_none()
            && self.italic.is_none()
            && self.underlined.is_none()
            && self.strikethrough.is_none()
            && self.obfuscated.is_none()
            && self.click_event.is_none()
            && self.hover_event.is_none()
            && self.extra.is_empty();
        if is_plain {
            return json!(self.text);
        }
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        // A translate component must not also carry an empty "text" key, and a
        // text component always needs "text" even when empty.
        if let Value::Object(map) = &mut value {
            if self.translate.is_none() && !map.contains_key("text") {
                map.insert("text".into(), json!(""));
            }
        }
        value
    }

    pub fn to_nbt(&self) -> NbtTag {
        NbtTag::from_json(&self.to_json())
    }

    pub fn from_json(value: &Value) -> Text {
        match value {
            Value::String(s) => Text::new(s.clone()),
            Value::Array(items) => {
                let mut parts: Vec<Text> = items.iter().map(Text::from_json).collect();
                if parts.is_empty() {
                    return Text::empty();
                }
                let mut first = parts.remove(0);
                first.extra.extend(parts);
                first
            }
            other => serde_json::from_value(other.clone()).unwrap_or_default(),
        }
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Self {
        Text::new(s)
    }
}

impl From<String> for Text {
    fn from(s: String) -> Self {
        Text::new(s)
    }
}

fn legacy_color(code: char) -> Option<&'static str> {
    Some(match code.to_ascii_lowercase() {
        '0' => "black",
        '1' => "dark_blue",
        '2' => "dark_green",
        '3' => "dark_aqua",
        '4' => "dark_red",
        '5' => "dark_purple",
        '6' => "gold",
        '7' => "gray",
        '8' => "dark_gray",
        '9' => "blue",
        'a' => "green",
        'b' => "aqua",
        'c' => "red",
        'd' => "light_purple",
        'e' => "yellow",
        'f' => "white",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_is_a_bare_string() {
        assert_eq!(Text::new("hi").to_json(), json!("hi"));
    }

    #[test]
    fn legacy_codes() {
        let t = Text::legacy("&aHello &lworld");
        assert_eq!(t.to_plain(), "Hello world");
        assert_eq!(t.extra[0].color.as_deref(), Some("green"));
        assert_eq!(t.extra[1].bold, Some(true));
        assert_eq!(t.extra[1].color.as_deref(), Some("green"));
    }
}
