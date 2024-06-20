use std::collections::HashMap;
use async_trait::async_trait;
use serde_json::Value;
use crate::at_commands::at_commands::AtCommandsContext;
use crate::at_tools::tools::AtTool;
use crate::call_validation::{ChatMessage, ContextEnum};


pub struct AtPatch {
}

#[async_trait]
impl AtTool for AtPatch {
    async fn execute(&self, ccx: &mut AtCommandsContext, tool_call_id: &String, args: &HashMap<String, Value>) -> Result<Vec<ContextEnum>, String>
    {
        let cache_dir = {
            let gcx_locked = ccx.global_context.read().await;
            gcx_locked.cache_dir.clone()
        };
        let notes_dir_path = cache_dir.join("notes");

        let path = match args.get("path") {
            Some(Value::String(s)) => s,
            Some(v) => { return Err(format!("argument `path` is not a string: {:?}", v)) },
            None => { return Err("argument `path` is not a string".to_string()) }
        };

        let mut op = match args.get("op") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => { return Err(format!("argument `op` is not a string: {:?}", v)) },
            None => { "".to_string() }
        };

        let mut todo = match args.get("todo") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => { return Err(format!("argument `todo` is not a string: {:?}", v)) },
            None => { "".to_string() }
        };

        // sit there until request is complete
        // ccx.messages
        // ccx.model
        // AtCommandsContext
        // ccx.messages;

        let mut results = vec![];
        results.push(ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: format!("Note saved"),
            tool_calls: None,
            tool_call_id: tool_call_id.clone(),
        }));
        Ok(results)
    }
}
