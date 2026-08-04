use std::sync::Arc;

use anyhow::{Result, bail};
use futures::io::AsyncReadExt;
use gpui::http_client::{AsyncBody, HttpClient, Method, Request};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::markdown::{Block, parse_blocks, strip_inline};

const NOTION_VERSION: &str = "2022-06-28";
const CODE_MARKER: &str = "\x60";
pub const CREDENTIALS_URL: &str = "open-live-writer://publishing";
pub const CREDENTIALS_USERNAME: &str = "open-live-writer";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct StoredPublishSettings {
    pub notion_token: String,
    pub notion_parent_page_id: String,
    pub typecho_xmlrpc_url: String,
    pub typecho_username: String,
    pub typecho_password: String,
}

impl StoredPublishSettings {
    pub fn notion_config(&self) -> Option<NotionConfig> {
        (!self.notion_token.is_empty() && !self.notion_parent_page_id.is_empty()).then(|| {
            NotionConfig {
                token: self.notion_token.clone(),
                parent_page_id: self.notion_parent_page_id.clone(),
            }
        })
    }

    pub fn typecho_config(&self) -> Option<TypechoConfig> {
        (!self.typecho_xmlrpc_url.is_empty()
            && !self.typecho_username.is_empty()
            && !self.typecho_password.is_empty())
        .then(|| TypechoConfig {
            xmlrpc_url: self.typecho_xmlrpc_url.clone(),
            username: self.typecho_username.clone(),
            password: self.typecho_password.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotionConfig {
    pub token: String,
    pub parent_page_id: String,
}

impl NotionConfig {
    pub fn from_env() -> Option<Self> {
        Some(Self {
            token: std::env::var("OPEN_LIVE_WRITER_NOTION_TOKEN").ok()?,
            parent_page_id: std::env::var("OPEN_LIVE_WRITER_NOTION_PARENT_PAGE_ID").ok()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypechoConfig {
    pub xmlrpc_url: String,
    pub username: String,
    pub password: String,
}

impl TypechoConfig {
    pub fn from_env() -> Option<Self> {
        Some(Self {
            xmlrpc_url: std::env::var("OPEN_LIVE_WRITER_TYPECHO_XMLRPC_URL").ok()?,
            username: std::env::var("OPEN_LIVE_WRITER_TYPECHO_USERNAME").ok()?,
            password: std::env::var("OPEN_LIVE_WRITER_TYPECHO_PASSWORD").ok()?,
        })
    }
}

pub async fn publish_to_notion(
    http: Arc<dyn HttpClient>,
    config: NotionConfig,
    title: &str,
    markdown: &str,
) -> Result<String> {
    let children = notion_blocks(markdown)?;
    let payload = json!({
        "parent": { "page_id": config.parent_page_id },
        "properties": {
            "title": {
                "title": [{ "type": "text", "text": { "content": title } }]
            }
        },
        "children": children,
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri("https://api.notion.com/v1/pages")
        .header("Authorization", format!("Bearer {}", config.token))
        .header("Notion-Version", NOTION_VERSION)
        .header("Content-Type", "application/json")
        .body(AsyncBody::from(serde_json::to_vec(&payload)?))?;
    let mut response = http.send(request).await?;
    let status = response.status();
    let body = read_body(&mut response).await?;
    if !status.is_success() {
        bail!(
            "Notion 返回 HTTP {}：{}",
            status.as_u16(),
            response_text(&body)
        );
    }
    let result: Value = serde_json::from_slice(&body)?;
    result["id"]
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Notion 返回成功但没有页面 ID"))
}

pub async fn publish_to_typecho(
    http: Arc<dyn HttpClient>,
    config: TypechoConfig,
    title: &str,
    markdown: &str,
) -> Result<String> {
    let body = metaweblog_new_post_xml(&config.username, &config.password, title, markdown);
    let request = Request::builder()
        .method(Method::POST)
        .uri(&config.xmlrpc_url)
        .header("Content-Type", "text/xml; charset=utf-8")
        .body(AsyncBody::from(body.into_bytes()))?;
    let mut response = http.send(request).await?;
    let status = response.status();
    let body = read_body(&mut response).await?;
    let text = String::from_utf8_lossy(&body);
    if !status.is_success() {
        bail!("Typecho 返回 HTTP {}：{}", status.as_u16(), text.trim());
    }
    if text.contains("<fault>") || text.contains("<boolean>0</boolean>") {
        bail!("Typecho XML-RPC 返回错误：{}", text.trim());
    }
    Ok(extract_xml_value(&text).unwrap_or_else(|| "已提交".to_owned()))
}

pub fn document_title(markdown: &str) -> String {
    parse_blocks(markdown)
        .into_iter()
        .find_map(|block| match block {
            Block::Heading { text, .. } => Some(strip_inline(&text)),
            Block::Paragraph(text) => Some(strip_inline(&text)),
            _ => None,
        })
        .filter(|title| !title.trim().is_empty())
        .map(|title| title.chars().take(100).collect())
        .unwrap_or_else(|| "未命名文章".to_owned())
}

pub fn notion_blocks(markdown: &str) -> Result<Vec<Value>> {
    let blocks = parse_blocks(markdown);
    if blocks.len() > 100 {
        bail!("Notion 单次发布最多支持 100 个区块，请拆分文章");
    }
    Ok(blocks.into_iter().map(block_to_notion).collect())
}

fn block_to_notion(block: Block) -> Value {
    match block {
        Block::Heading { level, text } => json!({
            "object": "block",
            "type": format!("heading_{}", level.clamp(1, 3)),
            format!("heading_{}", level.clamp(1, 3)): { "rich_text": rich_text(&text) }
        }),
        Block::Paragraph(text) => json!({
            "object": "block",
            "type": "paragraph",
            "paragraph": { "rich_text": rich_text(&text) }
        }),
        Block::Bullet(text) => json!({
            "object": "block",
            "type": "bulleted_list_item",
            "bulleted_list_item": { "rich_text": rich_text(&text) }
        }),
        Block::Numbered { text, .. } => json!({
            "object": "block",
            "type": "numbered_list_item",
            "numbered_list_item": { "rich_text": rich_text(&text) }
        }),
        Block::Quote(text) => json!({
            "object": "block",
            "type": "quote",
            "quote": { "rich_text": rich_text(&text) }
        }),
        Block::Code(text) => json!({
            "object": "block",
            "type": "code",
            "code": {
                "rich_text": rich_text(&text),
                "language": "plain text"
            }
        }),
        Block::Image { url, .. } => json!({
            "object": "block",
            "type": "image",
            "image": {
                "type": "external",
                "external": { "url": url }
            }
        }),
        Block::Video { url } => json!({
            "object": "block",
            "type": "video",
            "video": {
                "type": "external",
                "external": { "url": url }
            }
        }),
        Block::Table { headers, rows } => {
            let width = headers.len().max(1);
            let mut children = vec![notion_table_row(headers)];
            children.extend(rows.into_iter().map(notion_table_row));
            json!({
                "object": "block",
                "type": "table",
                "table": {
                    "table_width": width,
                    "has_column_header": true,
                    "has_row_header": false
                },
                "children": children
            })
        }
        Block::Divider => json!({
            "object": "block",
            "type": "divider",
            "divider": {}
        }),
    }
}

fn notion_table_row(cells: Vec<String>) -> Value {
    json!({
        "object": "block",
        "type": "table_row",
        "table_row": {
            "cells": cells.into_iter().map(|cell| rich_text(&cell)).collect::<Vec<_>>()
        }
    })
}

fn rich_text(text: &str) -> Vec<Value> {
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < text.len() {
        let rest = &text[offset..];
        let Some((marker_offset, marker)) = next_inline_marker(rest) else {
            push_rich_text(&mut result, rest, None, None);
            break;
        };
        if marker_offset > 0 {
            push_rich_text(&mut result, &rest[..marker_offset], None, None);
            offset += marker_offset;
            continue;
        }
        if marker == "[" {
            if let Some((label_end, url_end)) = linked_text_end(rest) {
                let label = &rest[1..label_end];
                let url = &rest[label_end + 2..url_end];
                push_rich_text(&mut result, label, None, Some(url));
                offset += url_end + 1;
                continue;
            }
        } else if let Some(close_offset) = rest[marker.len()..].find(marker) {
            let start = marker.len();
            let end = start + close_offset;
            let annotation = match marker {
                "**" | "__" => Some("bold"),
                "*" | "_" => Some("italic"),
                "~~" => Some("strikethrough"),
                CODE_MARKER => Some("code"),
                _ => None,
            };
            if let Some(annotation) = annotation {
                push_rich_text(&mut result, &rest[start..end], Some(annotation), None);
                offset += end + marker.len();
                continue;
            }
        }
        push_rich_text(&mut result, marker, None, None);
        offset += marker.len();
    }
    result
}

fn next_inline_marker(text: &str) -> Option<(usize, &'static str)> {
    ["**", "__", "~~", "*", "_", CODE_MARKER, "["]
        .into_iter()
        .filter_map(|marker| text.find(marker).map(|offset| (offset, marker)))
        .min_by_key(|(offset, _)| *offset)
}

fn linked_text_end(text: &str) -> Option<(usize, usize)> {
    let label_end = text.find("](")?;
    let url_end = text[label_end + 2..].find(')')? + label_end + 2;
    Some((label_end, url_end))
}

fn push_rich_text(
    result: &mut Vec<Value>,
    text: &str,
    annotation: Option<&str>,
    link: Option<&str>,
) {
    for chunk in text.chars().collect::<Vec<_>>().chunks(2000) {
        let content: String = chunk.iter().collect();
        let mut text_value = json!({ "content": content });
        if let Some(url) = link {
            text_value["link"] = json!({ "url": url });
        }
        let mut value = json!({ "type": "text", "text": text_value });
        if let Some(annotation) = annotation {
            value["annotations"] = json!({ annotation: true });
        }
        result.push(value);
    }
}

fn metaweblog_new_post_xml(username: &str, password: &str, title: &str, markdown: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <methodCall><methodName>metaWeblog.newPost</methodName><params>\
         <param><value><string>0</string></value></param>\
         <param><value><string>{}</string></value></param>\
         <param><value><string>{}</string></value></param>\
         <param><value><struct>\
         <member><name>title</name><value><string>{}</string></value></member>\
         <member><name>description</name><value><string>{}</string></value></member>\
         </struct></value></param>\
         <param><value><boolean>1</boolean></value></param>\
         </params></methodCall>",
        xml_escape(username),
        xml_escape(password),
        xml_escape(title),
        xml_escape(markdown),
    )
}

async fn read_body(response: &mut gpui::http_client::Response<AsyncBody>) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    response.body_mut().read_to_end(&mut body).await?;
    Ok(body)
}

fn response_text(body: &[u8]) -> String {
    String::from_utf8_lossy(body).chars().take(500).collect()
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}

fn extract_xml_value(response: &str) -> Option<String> {
    for tag in ["string", "int", "i4"] {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        if let Some(start) = response.find(&open) {
            let start = start + open.len();
            if let Some(end) = response[start..].find(&close) {
                return Some(response[start..start + end].to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{document_title, metaweblog_new_post_xml, notion_blocks, xml_escape};

    #[test]
    fn exports_notion_blocks_without_losing_ordered_list_semantics() {
        let blocks = notion_blocks("# 标题\n\n1. 第一项\n\n> 引用").unwrap();
        assert_eq!(blocks[0]["type"], "heading_1");
        assert_eq!(blocks[1]["type"], "numbered_list_item");
        assert_eq!(blocks[2]["type"], "quote");
    }

    #[test]
    fn preserves_common_inline_annotations_and_links() {
        let blocks = notion_blocks("**粗体** *斜体* [链接](https://example.com)").unwrap();
        let rich_text = &blocks[0]["paragraph"]["rich_text"];
        assert_eq!(rich_text[0]["annotations"]["bold"], true);
        assert_eq!(rich_text[2]["annotations"]["italic"], true);
        assert_eq!(rich_text[4]["text"]["link"]["url"], "https://example.com");
    }

    #[test]
    fn exports_images_and_tables_for_notion() {
        let blocks = notion_blocks(
            "![封面](https://example.com/a.png)\n\n[视频](https://example.com/video)\n\n| 名称 | 值 |\n| --- | --- |\n| 一 | 二 |",
        )
        .unwrap();
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(
            blocks[0]["image"]["external"]["url"],
            "https://example.com/a.png"
        );
        assert_eq!(blocks[1]["type"], "video");
        assert_eq!(
            blocks[1]["video"]["external"]["url"],
            "https://example.com/video"
        );
        assert_eq!(blocks[2]["type"], "table");
        assert_eq!(blocks[2]["table"]["table_width"], 2);
        assert_eq!(
            blocks[2]["children"][1]["table_row"]["cells"][0][0]["text"]["content"],
            "一"
        );
    }

    #[test]
    fn derives_a_stable_title_and_escapes_xml() {
        assert_eq!(document_title("# 我的文章\n\n正文"), "我的文章");
        assert_eq!(xml_escape("<a&\"'"), "&lt;a&amp;&quot;&apos;");
        let xml = metaweblog_new_post_xml("u&", "p", "标题", "正文");
        assert!(xml.contains("<string>u&amp;</string>"));
        assert!(xml.contains("<member><name>description</name>"));
    }
}
