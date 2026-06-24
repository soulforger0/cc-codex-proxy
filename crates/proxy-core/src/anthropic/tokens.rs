use crate::anthropic::schema::AnthropicRequest;

pub fn estimate_input_tokens(request: &AnthropicRequest) -> u64 {
    let mut chars = request.model.len();
    if let Some(system) = &request.system {
        chars += system.to_string().len();
    }
    for message in &request.messages {
        chars += message.role.len();
        chars += message.content.to_string().len();
    }
    if let Some(tools) = &request.tools {
        for tool in tools {
            chars += tool.name.len();
            chars += tool.description.as_deref().unwrap_or("").len();
            chars += tool
                .input_schema
                .as_ref()
                .map(|schema| schema.to_string().len())
                .unwrap_or(0);
        }
    }
    // Conservative, cheap estimate. Claude Code uses this for compaction decisions; over-counting is safer.
    ((chars as f64) / 3.5).ceil() as u64
}
