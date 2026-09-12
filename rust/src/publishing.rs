use std::sync::Arc;

use anyhow::{Result, bail};
use futures::io::AsyncReadExt;
use futures::{
    channel::oneshot,
    future::{BoxFuture, Either, select},
};
use gpui::Timer;
use gpui::http_client::{
    AsyncBody, HttpClient, Method, Request, Response, StatusCode, Url, http::HeaderValue,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::markdown::{Block, InlineStyle, parse_blocks, parse_inline, strip_inline};

const NOTION_VERSION: &str = "2026-03-11";
pub const CREDENTIALS_URL: &str = "open-live-writer://publishing";
pub const CREDENTIALS_USERNAME: &str = "open-live-writer";

const REQUEST_TIMEOUT_SECONDS: u64 = 30;
const HTTP_USER_AGENT: &str = "Open-Live-Writer/0.1";

/// GPUI's default application client is a deliberately blocked placeholder.
/// Install a real client before any publishing action is dispatched.
struct ReqwestHttpClient {
    client: reqwest::blocking::Client,
    user_agent: HeaderValue,
}

impl ReqwestHttpClient {
    fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::new(),
            user_agent: HeaderValue::from_static(HTTP_USER_AGENT),
        }
    }
}

pub fn default_http_client() -> Arc<dyn HttpClient> {
    Arc::new(ReqwestHttpClient::new())
}

impl HttpClient for ReqwestHttpClient {
    fn type_name(&self) -> &'static str {
        "open_live_writer::ReqwestHttpClient"
    }

    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let client = self.client.clone();
        Box::pin(async move {
            let (parts, mut body) = request.into_parts();
            let mut request_body = Vec::new();
            body.read_to_end(&mut request_body).await?;

            let (sender, receiver) = oneshot::channel();
            std::thread::Builder::new()
                .name("open-live-writer-http".to_owned())
                .spawn(move || {
                    let _ = sender.send(send_with_reqwest(client, parts, request_body));
                })?;
            receiver
                .await
                .map_err(|_| anyhow::anyhow!("HTTP 请求线程意外退出"))?
        })
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }
}

fn send_with_reqwest(
    client: reqwest::blocking::Client,
    parts: gpui::http_client::http::request::Parts,
    request_body: Vec<u8>,
) -> anyhow::Result<Response<AsyncBody>> {
    let method = reqwest::Method::from_bytes(parts.method.as_str().as_bytes())?;
    let mut request_builder = client
        .request(method, parts.uri.to_string())
        .body(request_body);
    for (name, value) in &parts.headers {
        request_builder = request_builder.header(name.as_str(), value.as_bytes());
    }

    let response = request_builder.send()?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let response_body = response.bytes()?;

    let mut response_builder = Response::builder().status(status);
    for (name, value) in &headers {
        response_builder = response_builder.header(name.as_str(), value.as_bytes());
    }
    Ok(response_builder.body(AsyncBody::from(response_body.to_vec()))?)
}

/// 给发布请求加超时，避免服务端无响应时永久挂起。
async fn request_with_timeout<T, F>(future: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    let timer = async {
        Timer::after(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECONDS)).await;
    };
    match select(Box::pin(future), Box::pin(timer)).await {
        Either::Left((result, _)) => result,
        Either::Right(_) => bail!("发布请求超时（{REQUEST_TIMEOUT_SECONDS} 秒无响应），请稍后重试"),
    }
}

fn env_var_trimmed(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

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
        if self.notion_token.trim().is_empty() || self.notion_parent_page_id.trim().is_empty() {
            None
        } else {
            Some(NotionConfig {
                token: self.notion_token.trim().to_owned(),
                parent_page_id: self.notion_parent_page_id.trim().to_owned(),
            })
        }
    }

    pub fn typecho_config(&self) -> Option<TypechoConfig> {
        if self.typecho_xmlrpc_url.trim().is_empty()
            || self.typecho_username.trim().is_empty()
            || self.typecho_password.trim().is_empty()
        {
            None
        } else {
            Some(TypechoConfig {
                xmlrpc_url: self.typecho_xmlrpc_url.trim().to_owned(),
                username: self.typecho_username.trim().to_owned(),
                password: self.typecho_password.clone(),
            })
        }
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
            token: env_var_trimmed("OPEN_LIVE_WRITER_NOTION_TOKEN")?,
            parent_page_id: env_var_trimmed("OPEN_LIVE_WRITER_NOTION_PARENT_PAGE_ID")?,
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
            xmlrpc_url: env_var_trimmed("OPEN_LIVE_WRITER_TYPECHO_XMLRPC_URL")?,
            username: env_var_trimmed("OPEN_LIVE_WRITER_TYPECHO_USERNAME")?,
            password: env_var_trimmed("OPEN_LIVE_WRITER_TYPECHO_PASSWORD")?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NotionTarget {
    id: String,
    database_hint: bool,
}

impl NotionTarget {
    fn parse(input: &str) -> Self {
        let input = input.trim();
        let query = input.split_once('?').map(|(_, query)| query).unwrap_or("");
        let database_hint = query
            .split('&')
            .any(|parameter| parameter == "v" || parameter.starts_with("v="));
        let path = input.split(['?', '#']).next().unwrap_or(input);
        let id = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .to_owned();
        Self { id, database_hint }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotionPublishResult {
    pub id: String,
    pub url: String,
}

pub async fn publish_to_notion(
    http: Arc<dyn HttpClient>,
    config: NotionConfig,
    title: &str,
    markdown: &str,
) -> Result<NotionPublishResult> {
    let target = NotionTarget::parse(&config.parent_page_id);
    if target.id.is_empty() {
        bail!("Notion parent page or database ID is empty");
    }
    let children = notion_blocks(markdown)?;
    if target.database_hint {
        return publish_to_notion_data_source(http, &config, &target.id, title, children).await;
    }

    let payload = notion_page_payload(
        json!({ "page_id": target.id }),
        "title",
        title,
        children.clone(),
    );
    let request = notion_request(
        &config.token,
        Method::POST,
        "https://api.notion.com/v1/pages".to_owned(),
        Some(payload),
    )?;
    let (status, body) = send_notion_request(http.clone(), request).await?;
    if status.is_success() {
        return parse_notion_page_id(status, &body);
    }
    if status.as_u16() != 404 {
        bail!(
            "Notion returned HTTP {}: {}",
            status.as_u16(),
            response_text(&body)
        );
    }

    match publish_to_notion_data_source(http, &config, &target.id, title, children).await {
        Ok(page) => Ok(page),
        Err(database_error) => bail!(
            "Notion returned HTTP 404: the parent page was not found; database/data-source fallback also failed: {database_error}"
        ),
    }
}

async fn publish_to_notion_data_source(
    http: Arc<dyn HttpClient>,
    config: &NotionConfig,
    database_id: &str,
    title: &str,
    children: Vec<Value>,
) -> Result<NotionPublishResult> {
    let data_source_id = resolve_notion_data_source(http.clone(), config, database_id).await?;
    let title_property = notion_title_property(http.clone(), config, &data_source_id).await?;
    let payload = notion_page_payload(
        json!({ "data_source_id": data_source_id }),
        &title_property,
        title,
        children,
    );
    let request = notion_request(
        &config.token,
        Method::POST,
        "https://api.notion.com/v1/pages".to_owned(),
        Some(payload),
    )?;
    let (status, body) = send_notion_request(http, request).await?;
    parse_notion_page_id(status, &body)
}

async fn resolve_notion_data_source(
    http: Arc<dyn HttpClient>,
    config: &NotionConfig,
    target_id: &str,
) -> Result<String> {
    let database_request = notion_request(
        &config.token,
        Method::GET,
        format!("https://api.notion.com/v1/databases/{target_id}"),
        None,
    )?;
    let (database_status, database_body) =
        send_notion_request(http.clone(), database_request).await?;
    if database_status.is_success() {
        let database: Value = serde_json::from_slice(&database_body)?;
        return database["data_sources"]
            .as_array()
            .and_then(|data_sources| {
                data_sources
                    .iter()
                    .find_map(|data_source| data_source["id"].as_str())
            })
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow::anyhow!("Notion database has no available data source"));
    }
    if database_status.as_u16() != 404 {
        bail!(
            "Notion database lookup returned HTTP {}: {}",
            database_status.as_u16(),
            response_text(&database_body)
        );
    }

    let data_source_request = notion_request(
        &config.token,
        Method::GET,
        format!("https://api.notion.com/v1/data_sources/{target_id}"),
        None,
    )?;
    let (data_source_status, data_source_body) =
        send_notion_request(http, data_source_request).await?;
    if data_source_status.is_success() {
        return Ok(target_id.to_owned());
    }
    bail!(
        "database HTTP {}: {}; data source HTTP {}: {}",
        database_status.as_u16(),
        response_text(&database_body),
        data_source_status.as_u16(),
        response_text(&data_source_body)
    );
}

async fn notion_title_property(
    http: Arc<dyn HttpClient>,
    config: &NotionConfig,
    data_source_id: &str,
) -> Result<String> {
    let request = notion_request(
        &config.token,
        Method::GET,
        format!("https://api.notion.com/v1/data_sources/{data_source_id}"),
        None,
    )?;
    let (status, body) = send_notion_request(http, request).await?;
    if !status.is_success() {
        bail!(
            "Notion data-source lookup returned HTTP {}: {}",
            status.as_u16(),
            response_text(&body)
        );
    }
    let data_source: Value = serde_json::from_slice(&body)?;
    data_source["properties"]
        .as_object()
        .and_then(|properties| {
            properties.iter().find_map(|(name, property)| {
                (property["type"] == "title" || property.get("title").is_some())
                    .then(|| name.to_owned())
            })
        })
        .ok_or_else(|| anyhow::anyhow!("Notion data source has no title property"))
}

fn notion_page_payload(
    parent: Value,
    title_property: &str,
    title: &str,
    children: Vec<Value>,
) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        title_property.to_owned(),
        json!({
            "type": "title",
            "title": [{ "type": "text", "text": { "content": title } }]
        }),
    );
    json!({
        "parent": parent,
        "properties": Value::Object(properties),
        "children": children,
    })
}

fn notion_request(
    token: &str,
    method: Method,
    uri: String,
    payload: Option<Value>,
) -> Result<Request<AsyncBody>> {
    let body = match payload {
        Some(payload) => serde_json::to_vec(&payload)?,
        None => Vec::new(),
    };
    Ok(Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {token}"))
        .header("Notion-Version", NOTION_VERSION)
        .header("Content-Type", "application/json")
        .body(AsyncBody::from(body))?)
}

async fn send_notion_request(
    http: Arc<dyn HttpClient>,
    request: Request<AsyncBody>,
) -> Result<(StatusCode, Vec<u8>)> {
    request_with_timeout(async {
        let mut response = http.send(request).await?;
        let status = response.status();
        let body = read_body(&mut response).await?;
        Ok::<_, anyhow::Error>((status, body))
    })
    .await
}

fn parse_notion_page_id(status: StatusCode, body: &[u8]) -> Result<NotionPublishResult> {
    if !status.is_success() {
        bail!(
            "Notion returned HTTP {}: {}",
            status.as_u16(),
            response_text(body)
        );
    }
    let result: Value = serde_json::from_slice(body)?;
    let id = result["id"]
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Notion returned success without a page ID"))?;
    let url = result["url"]
        .as_str()
        .filter(|url| !url.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Notion returned success without a page URL"))?;
    Ok(NotionPublishResult { id, url })
}

#[allow(dead_code)]
pub async fn publish_to_typecho(
    http: Arc<dyn HttpClient>,
    config: TypechoConfig,
    title: &str,
    markdown: &str,
) -> Result<String> {
    publish_or_update_typecho(http, config, title, markdown, None, true).await
}

pub async fn publish_or_update_typecho(
    http: Arc<dyn HttpClient>,
    config: TypechoConfig,
    title: &str,
    markdown: &str,
    existing_id: Option<&str>,
    publish: bool,
) -> Result<String> {
    let html = markdown_to_html(markdown);
    let body = if let Some(post_id) = existing_id.filter(|id| !id.trim().is_empty()) {
        metaweblog_edit_post_xml(
            post_id,
            &config.username,
            &config.password,
            title,
            &html,
            publish,
        )
    } else {
        metaweblog_new_post_xml(&config.username, &config.password, title, &html, publish)
    };
    let request = Request::builder()
        .method(Method::POST)
        .uri(&config.xmlrpc_url)
        .header("Content-Type", "text/xml; charset=utf-8")
        .body(AsyncBody::from(body.into_bytes()))?;
    let (status, body) = request_with_timeout(async {
        let mut response = http.send(request).await?;
        let status = response.status();
        let body = read_body(&mut response).await?;
        Ok::<_, anyhow::Error>((status, body))
    })
    .await?;
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
    let blocks = parse_blocks(markdown);
    let heading_title = blocks.iter().find_map(|block| match block {
        Block::Heading { text, .. } => Some(strip_inline(text)),
        _ => None,
    });
    let paragraph_title = blocks.into_iter().find_map(|block| match block {
        Block::Paragraph(text) => Some(strip_inline(&text)),
        _ => None,
    });
    heading_title
        .or(paragraph_title)
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

pub fn local_image_count(markdown: &str) -> usize {
    let file_url_count = markdown.to_ascii_lowercase().matches("file://").count();
    let extra = local_images(markdown)
        .iter()
        .filter(|image| !image.url.to_ascii_lowercase().contains("file://"))
        .count();
    file_url_count + extra
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalImage {
    pub alt: String,
    pub url: String,
}

pub fn local_images(markdown: &str) -> Vec<LocalImage> {
    parse_blocks(markdown)
        .into_iter()
        .filter_map(|block| match block {
            Block::Image { alt, url } if !is_public_url(&url) => Some(LocalImage { alt, url }),
            _ => None,
        })
        .collect()
}

#[allow(dead_code)]
pub fn replace_image_url(markdown: &str, from: &str, to: &str) -> String {
    markdown.replace(&format!("]({from})"), &format!("]({to})"))
}

/// Typecho's MetaWeblog endpoint renders the post body as HTML, so convert the
/// Markdown source before sending it.
pub fn markdown_to_html(markdown: &str) -> String {
    markdown::to_html_with_options(markdown, &markdown::Options::gfm())
        .unwrap_or_else(|_| markdown.to_owned())
}

fn notion_code_language(language: Option<&str>) -> &'static str {
    let Some(language) = language else {
        return "plain text";
    };
    match language.to_ascii_lowercase().as_str() {
        "js" | "javascript" => "javascript",
        "ts" | "typescript" => "typescript",
        "python" | "py" => "python",
        "rust" | "rs" => "rust",
        "go" | "golang" => "go",
        "c" => "c",
        "cpp" | "c++" => "cpp",
        "java" => "java",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "markdown" | "md" => "markdown",
        "html" => "html",
        "css" => "css",
        "shell" | "sh" | "bash" | "zsh" => "bash",
        "sql" => "sql",
        "swift" => "swift",
        "kotlin" => "kotlin",
        "ruby" | "rb" => "ruby",
        "php" => "php",
        "dart" => "dart",
        "xml" => "xml",
        _ => "plain text",
    }
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
        Block::Bullet { text, .. } => json!({
            "object": "block",
            "type": "bulleted_list_item",
            "bulleted_list_item": { "rich_text": rich_text(&text) }
        }),
        Block::Numbered { text, .. } => json!({
            "object": "block",
            "type": "numbered_list_item",
            "numbered_list_item": { "rich_text": rich_text(&text) }
        }),
        Block::Task { checked, text, .. } => json!({
            "object": "block",
            "type": "to_do",
            "to_do": { "rich_text": rich_text(&text), "checked": checked }
        }),
        Block::Quote(text) => json!({
            "object": "block",
            "type": "quote",
            "quote": { "rich_text": rich_text(&text) }
        }),
        Block::Aside(text) => json!({
            "object": "block",
            "type": "callout",
            "callout": {
                "rich_text": rich_text(&text),
                "icon": { "type": "emoji", "emoji": "💡" }
            }
        }),
        Block::Code { text, language } => json!({
            "object": "block",
            "type": "code",
            "code": {
                "rich_text": plain_rich_text(&text),
                "language": notion_code_language(language.as_deref())
            }
        }),
        Block::Image { alt, url } => {
            if is_public_url(&url) {
                json!({
                    "object": "block",
                    "type": "image",
                    "image": {
                        "type": "external",
                        "external": { "url": url }
                    }
                })
            } else {
                non_public_media_note("图片", alt.as_str(), &url)
            }
        }
        Block::Video { url } => {
            if is_public_url(&url) {
                json!({
                    "object": "block",
                    "type": "video",
                    "video": {
                        "type": "external",
                        "external": { "url": url }
                    }
                })
            } else {
                non_public_media_note("视频", "", &url)
            }
        }
        Block::Table { headers, rows } => {
            let width = std::iter::once(headers.len())
                .chain(rows.iter().map(Vec::len))
                .max()
                .unwrap_or(1)
                .max(1);
            let mut children = vec![notion_table_row(pad_table_cells(headers, width))];
            children.extend(
                rows.into_iter()
                    .map(|row| notion_table_row(pad_table_cells(row, width))),
            );
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
        Block::Html(text) => json!({
            "object": "block",
            "type": "paragraph",
            "paragraph": { "rich_text": rich_text(&text) }
        }),
    }
}

fn is_public_url(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    url.starts_with("https://") || url.starts_with("http://")
}

#[allow(dead_code)]
fn is_file_url(url: &str) -> bool {
    url.trim().to_ascii_lowercase().starts_with("file://")
}

fn non_public_media_note(kind: &str, label: &str, url: &str) -> Value {
    let label = label.trim();
    let text = if label.is_empty() {
        format!("{kind}未上传：{url}")
    } else {
        format!("{kind}未上传：{label}（{url}）")
    };
    json!({
        "object": "block",
        "type": "paragraph",
        "paragraph": { "rich_text": rich_text(&text) }
    })
}

fn pad_table_cells(mut cells: Vec<String>, width: usize) -> Vec<String> {
    if cells.len() < width {
        cells.extend(std::iter::repeat_with(String::new).take(width - cells.len()));
    }
    cells.truncate(width);
    cells
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
    for piece in parse_inline(text) {
        if piece.text.is_empty() {
            continue;
        }
        push_rich_text(
            &mut result,
            &piece.text,
            &piece.styles,
            piece.link.as_deref(),
        );
    }
    result
}

fn plain_rich_text(text: &str) -> Vec<Value> {
    let mut result = Vec::new();
    push_rich_text(&mut result, text, &[], None);
    result
}

fn push_rich_text(result: &mut Vec<Value>, text: &str, styles: &[InlineStyle], link: Option<&str>) {
    for chunk in text.chars().collect::<Vec<_>>().chunks(2000) {
        let content: String = chunk.iter().collect();
        let mut text_value = json!({ "content": content });
        if let Some(url) = link {
            text_value["link"] = json!({ "url": url });
        }
        let mut value = json!({ "type": "text", "text": text_value });
        for style in styles {
            let annotation = match style {
                InlineStyle::Bold => "bold",
                InlineStyle::Italic => "italic",
                InlineStyle::Strike => "strikethrough",
                InlineStyle::Code => "code",
            };
            value["annotations"][annotation] = json!(true);
        }
        result.push(value);
    }
}

fn metaweblog_post_xml(
    method: &str,
    first_id: &str,
    username: &str,
    password: &str,
    title: &str,
    markdown: &str,
    publish: bool,
) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <methodCall><methodName>{method}</methodName><params>\
         <param><value><string>{}</string></value></param>\
         <param><value><string>{}</string></value></param>\
         <param><value><string>{}</string></value></param>\
         <param><value><struct>\
         <member><name>title</name><value><string>{}</string></value></member>\
         <member><name>description</name><value><string>{}</string></value></member>\
         </struct></value></param>\
         <param><value><boolean>{}</boolean></value></param>\
         </params></methodCall>",
        xml_escape(first_id),
        xml_escape(username),
        xml_escape(password),
        xml_escape(title),
        xml_escape(markdown),
        if publish { "1" } else { "0" },
    )
}

fn metaweblog_new_post_xml(
    username: &str,
    password: &str,
    title: &str,
    markdown: &str,
    publish: bool,
) -> String {
    metaweblog_post_xml(
        "metaWeblog.newPost",
        "0",
        username,
        password,
        title,
        markdown,
        publish,
    )
}

fn metaweblog_edit_post_xml(
    post_id: &str,
    username: &str,
    password: &str,
    title: &str,
    markdown: &str,
    publish: bool,
) -> String {
    metaweblog_post_xml(
        "metaWeblog.editPost",
        post_id,
        username,
        password,
        title,
        markdown,
        publish,
    )
}

pub fn encode_base64(bytes: &[u8]) -> String {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut index = 0;
    while index < bytes.len() {
        let remaining = bytes.len() - index;
        let b0 = bytes[index];
        let b1 = if remaining > 1 { bytes[index + 1] } else { 0 };
        let b2 = if remaining > 2 { bytes[index + 2] } else { 0 };
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        if remaining > 1 {
            out.push(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if remaining > 2 {
            out.push(TABLE[(b2 & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        index += 3;
    }
    out
}

#[allow(dead_code)]
pub async fn upload_typecho_media(
    http: Arc<dyn HttpClient>,
    config: TypechoConfig,
    file_name: &str,
    mime: &str,
    bytes: &[u8],
) -> Result<String> {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><methodCall><methodName>metaWeblog.newMediaObject</methodName><params><param><value><string>0</string></value></param><param><value><string>{}</string></value></param><param><value><string>{}</string></value></param><param><value><struct><member><name>name</name><value><string>{}</string></value></member><member><name>type</name><value><string>{}</string></value></member><member><name>bits</name><value><base64>{}</base64></value></member></struct></value></param></params></methodCall>",
        xml_escape(&config.username),
        xml_escape(&config.password),
        xml_escape(file_name),
        xml_escape(mime),
        encode_base64(bytes),
    );
    let request = Request::builder()
        .method(Method::POST)
        .uri(&config.xmlrpc_url)
        .header("Content-Type", "text/xml; charset=utf-8")
        .body(AsyncBody::from(body.into_bytes()))?;
    let (status, body) = request_with_timeout(async {
        let mut response = http.send(request).await?;
        let status = response.status();
        let body = read_body(&mut response).await?;
        Ok::<_, anyhow::Error>((status, body))
    })
    .await?;
    let text = String::from_utf8_lossy(&body);
    if !status.is_success() {
        bail!("Typecho media HTTP {}: {}", status.as_u16(), text.trim());
    }
    if text.contains("<fault>") {
        bail!("Typecho media XML-RPC error: {}", text.trim());
    }
    extract_xml_tag(&text, "string")
        .or_else(|| extract_xml_tag(&text, "url"))
        .ok_or_else(|| anyhow::anyhow!("Typecho media upload returned no URL"))
}

pub async fn test_notion_connection(
    http: Arc<dyn HttpClient>,
    config: NotionConfig,
) -> Result<String> {
    let request = notion_request(
        &config.token,
        Method::GET,
        "https://api.notion.com/v1/users/me".to_owned(),
        None,
    )?;
    let (status, body) = send_notion_request(http, request).await?;
    if !status.is_success() {
        bail!(
            "Notion test HTTP {}: {}",
            status.as_u16(),
            response_text(&body)
        );
    }
    let value: Value = serde_json::from_slice(&body)?;
    let name = value["name"]
        .as_str()
        .or_else(|| value["bot"]["owner"]["user"]["name"].as_str())
        .unwrap_or("Notion");
    Ok(name.to_owned())
}

pub async fn test_typecho_connection(
    http: Arc<dyn HttpClient>,
    config: TypechoConfig,
) -> Result<String> {
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><methodCall><methodName>blogger.getUsersBlogs</methodName><params><param><value><string>0</string></value></param><param><value><string>{}</string></value></param><param><value><string>{}</string></value></param></params></methodCall>",
        xml_escape(&config.username),
        xml_escape(&config.password),
    );
    let request = Request::builder()
        .method(Method::POST)
        .uri(&config.xmlrpc_url)
        .header("Content-Type", "text/xml; charset=utf-8")
        .body(AsyncBody::from(body.into_bytes()))?;
    let (status, body) = request_with_timeout(async {
        let mut response = http.send(request).await?;
        let status = response.status();
        let body = read_body(&mut response).await?;
        Ok::<_, anyhow::Error>((status, body))
    })
    .await?;
    let text = String::from_utf8_lossy(&body);
    if !status.is_success() {
        bail!("Typecho test HTTP {}: {}", status.as_u16(), text.trim());
    }
    if text.contains("<fault>") {
        bail!("Typecho test XML-RPC error: {}", text.trim());
    }
    Ok(extract_xml_tag(&text, "string").unwrap_or_else(|| "Typecho".to_owned()))
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

fn extract_xml_tag(response: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = response.find(&open)? + open.len();
    let end = response[start..].find(&close)?;
    Some(response[start..start + end].to_owned())
}

fn extract_xml_value(response: &str) -> Option<String> {
    for tag in ["string", "int", "i4"] {
        if let Some(value) = extract_xml_tag(response, tag) {
            return Some(value);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use futures::executor::block_on;
    use gpui::http_client::{AsyncBody, Method, Request};
    use serde_json::json;

    use super::{
        NotionTarget, default_http_client, document_title, local_image_count, markdown_to_html,
        metaweblog_new_post_xml, notion_blocks, notion_page_payload, read_body, xml_escape,
    };

    #[test]
    fn default_http_client_sends_requests_without_a_tokio_runtime() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let size = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("POST / HTTP/1.1"));
            assert!(request.contains("hello"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
        });

        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("http://{address}/"))
            .body(AsyncBody::from(b"hello".to_vec()))
            .unwrap();
        let mut response = block_on(default_http_client().send(request)).unwrap();
        let body = block_on(read_body(&mut response)).unwrap();

        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(body, b"ok");
        server.join().unwrap();
    }

    #[test]
    fn parses_notion_page_and_database_targets() {
        let database = NotionTarget::parse(
            "https://app.notion.com/p/tooktang/14c856c35cbc80a1aa43eb1c4955c328?v=28fcd53b378a47bfb371e339f0976aff",
        );
        assert_eq!(database.id, "14c856c35cbc80a1aa43eb1c4955c328");
        assert!(database.database_hint);

        let page = NotionTarget::parse("14c856c35cbc80a1aa43eb1c4955c328");
        assert_eq!(page.id, "14c856c35cbc80a1aa43eb1c4955c328");
        assert!(!page.database_hint);
    }

    #[test]
    fn builds_database_page_payload_with_dynamic_title_property() {
        let payload = notion_page_payload(
            json!({ "data_source_id": "data-source-id" }),
            "Name",
            "Article title",
            Vec::new(),
        );
        assert_eq!(payload["parent"]["data_source_id"], "data-source-id");
        assert_eq!(
            payload["properties"]["Name"]["title"][0]["text"]["content"],
            "Article title"
        );
    }

    #[test]
    fn exports_notion_blocks_without_losing_ordered_list_semantics() {
        let blocks = notion_blocks("# 标题\n\n1. 第一项\n\n> 引用").unwrap();
        assert_eq!(blocks[0]["type"], "heading_1");
        assert_eq!(blocks[1]["type"], "numbered_list_item");
        assert_eq!(blocks[2]["type"], "quote");
    }

    #[test]
    fn exports_gfm_tasks_and_notion_asides() {
        let blocks = notion_blocks("- [ ] 待办\n- [x] 已完成\n\n<aside>提示</aside>").unwrap();
        assert_eq!(blocks[0]["type"], "to_do");
        assert_eq!(blocks[0]["to_do"]["checked"], false);
        assert_eq!(blocks[1]["to_do"]["checked"], true);
        assert_eq!(blocks[2]["type"], "callout");
        assert_eq!(
            blocks[2]["callout"]["rich_text"][0]["text"]["content"],
            "提示"
        );
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
    fn converts_markdown_to_html_for_typecho() {
        let html = markdown_to_html("# 标题\n\n- 一\n- 二");
        assert!(html.contains("<h1>标题</h1>"));
        assert!(html.contains("<li>一</li>"));
    }

    #[test]
    fn counts_local_image_links() {
        assert_eq!(
            local_image_count("![a](file:///C:/x.png) [b](https://e.com)"),
            1
        );
        assert_eq!(local_image_count("no local images"), 0);
    }

    #[test]
    fn exports_code_language_for_notion() {
        let blocks = notion_blocks("```rust\nfn main() {}\n```").unwrap();
        assert_eq!(blocks[0]["code"]["language"], "rust");
        let blocks = notion_blocks("```\nplain\n```").unwrap();
        assert_eq!(blocks[0]["code"]["language"], "plain text");
        let blocks = notion_blocks("```UnknownLang\nx\n```").unwrap();
        assert_eq!(blocks[0]["code"]["language"], "plain text");
    }

    #[test]
    fn derives_a_stable_title_and_escapes_xml() {
        assert_eq!(document_title("# 我的文章\n\n正文"), "我的文章");
        assert_eq!(document_title("开头说明\n\n# 正式标题"), "正式标题");
        assert_eq!(xml_escape("<a&\"'"), "&lt;a&amp;&quot;&apos;");
        let xml = metaweblog_new_post_xml("u&", "p", "标题", "正文", true);
        assert!(xml.contains("<string>u&amp;</string>"));
        assert!(xml.contains("<member><name>description</name>"));
    }

    #[test]
    fn encodes_base64_and_lists_local_images() {
        assert_eq!(super::encode_base64(b"Man"), "TWFu");
        assert_eq!(super::local_images("![a](file:///C:/x.png)").len(), 1);
    }
}
