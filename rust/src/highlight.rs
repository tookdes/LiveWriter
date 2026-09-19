use std::sync::Arc;

use gpui::App;
use gpui_component::{
    Theme,
    highlighter::{LanguageConfig, LanguageRegistry, ThemeStyle},
};

pub fn register_markdown_highlighter() {
    let registry = LanguageRegistry::singleton();
    let markdown = LanguageConfig::new(
        "markdown",
        tree_sitter_md::LANGUAGE.into(),
        vec!["markdown_inline".into()],
        include_str!("../queries/markdown/highlights.scm"),
        include_str!("../queries/markdown/injections.scm"),
        "",
    );
    let markdown_inline = LanguageConfig::new(
        "markdown_inline",
        tree_sitter_md::INLINE_LANGUAGE.into(),
        vec![],
        include_str!("../queries/markdown_inline/highlights.scm"),
        "",
        "",
    );
    registry.register("markdown", &markdown);
    registry.register("md", &markdown);
    registry.register("markdown_inline", &markdown_inline);
    registry.register("markdown-inline", &markdown_inline);
}

fn syntax_style(value: serde_json::Value) -> ThemeStyle {
    serde_json::from_value(value).expect("valid markdown syntax style")
}

pub fn apply_article_syntax_theme(cx: &mut App) {
    let mut highlight = (*Theme::global(cx).highlight_theme).clone();
    highlight.name = "Open Live Writer Markdown".into();
    let syntax = &mut highlight.style.syntax;
    syntax.title = Some(syntax_style(serde_json::json!({
        "color": "#203C59",
        "font_weight": 700
    })));
    syntax.emphasis = Some(syntax_style(serde_json::json!({
        "font_style": "italic"
    })));
    syntax.emphasis_strong = Some(syntax_style(serde_json::json!({
        "font_weight": 700
    })));
    syntax.punctuation_special = Some(syntax_style(serde_json::json!({
        "color": "#8796A5"
    })));
    syntax.punctuation_list_marker = Some(syntax_style(serde_json::json!({
        "color": "#3B78B4",
        "font_weight": 700
    })));
    syntax.punctuation_delimiter = Some(syntax_style(serde_json::json!({
        "color": "#8796A5"
    })));
    syntax.text_literal = Some(syntax_style(serde_json::json!({
        "color": "#6F42C1"
    })));
    syntax.link_text = Some(syntax_style(serde_json::json!({
        "color": "#2B5F95"
    })));
    syntax.link_uri = Some(syntax_style(serde_json::json!({
        "color": "#617285",
        "font_style": "italic"
    })));
    syntax.comment = Some(syntax_style(serde_json::json!({
        "color": "#8796A5"
    })));
    Theme::global_mut(cx).highlight_theme = Arc::new(highlight);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn article_syntax_style_accepts_hex_and_weight() {
        let style = syntax_style(serde_json::json!({
            "color": "#203C59",
            "font_weight": 700,
            "font_style": "italic"
        }));
        let highlight = gpui::HighlightStyle::from(style);
        assert!(highlight.color.is_some());
        assert!(highlight.font_weight.is_some());
        assert_eq!(highlight.font_style, Some(gpui::FontStyle::Italic));
    }

    #[test]
    fn registers_markdown_without_the_full_language_pack() {
        register_markdown_highlighter();
        let registry = LanguageRegistry::singleton();
        assert!(registry.language("markdown").is_some());
        assert!(registry.language("markdown_inline").is_some());
        assert!(registry.language("json").is_some());
    }
}
