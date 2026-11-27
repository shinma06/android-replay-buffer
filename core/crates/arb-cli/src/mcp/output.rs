//! MCP tool result envelopes and the negotiated representation of resource references.
use super::{resources::ResourceLink, version::ProtocolVersion};
use serde_json::{Value, json};

#[derive(Debug)]
pub(crate) struct ToolOutput {
    pub(crate) body: Value,
    image: Option<Value>,
    links: Option<Vec<ResourceLink>>,
}

impl ToolOutput {
    pub(super) const fn new(body: Value) -> Self {
        Self {
            body,
            image: None,
            links: None,
        }
    }
    pub(super) fn with_image(mut self, image: Value) -> Self {
        self.image = Some(image);
        self
    }
    pub(super) fn with_links(mut self, links: Vec<ResourceLink>) -> Self {
        self.links = Some(links);
        self
    }
    pub(super) fn add_link(&mut self, link: ResourceLink) {
        self.links.get_or_insert_with(Vec::new).push(link);
    }
    pub(super) fn result(mut self, version: ProtocolVersion) -> Value {
        if let Some(links) = &self.links {
            self.body["resource_links"] = json!(links);
        }
        let mut content = vec![text_block(&self.body)];
        content.extend(self.image);
        if version.supports_resource_link_block() {
            content.extend(self.links.iter().flatten().map(ResourceLink::block));
        }
        let mut result = json!({"isError":false,"content":content});
        if version.supports_structured_content() {
            result["structuredContent"] = self.body;
        }
        result
    }
}

fn text_block(body: &Value) -> Value {
    json!({"type":"text","text":body.to_string()})
}
pub(super) fn error(body: Value, version: ProtocolVersion) -> Value {
    let mut result = json!({"isError":true,"content":[text_block(&body)]});
    if version.supports_structured_content() {
        result["structuredContent"] = body;
    }
    result
}
