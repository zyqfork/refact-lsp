use std::collections::HashMap;
use async_trait::async_trait;
use serde_json::Value;
use crate::at_commands::at_commands::AtCommandsContext;
use crate::at_tools::tools::Tool;
use crate::call_validation::{ChatMessage, ContextEnum};
use crate::scratchpads;
use tracing::{info, warn};
use crate::call_validation::{ChatPost, SamplingParameters};


pub struct ToolPatch {
}


const PATCH_SYSTEM_PROMPT: &str = r#"
You are a diff generator.
Use this format:

<<<<<<<< SEARCH
original code
========
replacement code
>>>>>>>> REPLACE

In addition to changing the existing code, you are also responsible for adding and removing entire files.

To add a file write:

<<<<<<<< NEW
new file code
>>>>>>>> END

To remove a file:

<<<<<<<< REMOVE
>>>>>>>> END
"#;


#[async_trait]
impl Tool for ToolPatch {
    async fn execute(&self, ccx: &mut AtCommandsContext, tool_call_id: &String, args: &HashMap<String, Value>) -> Result<Vec<ContextEnum>, String>
    {
        let cache_dir = {
            let gcx_locked = ccx.global_context.read().await;
            gcx_locked.cache_dir.clone()
        };
        // let notes_dir_path = cache_dir.join("notes");

        let path = match args.get("path") {
            Some(Value::String(s)) => s,
            Some(v) => { return Err(format!("argument `path` is not a string: {:?}", v)) },
            None => { return Err("argument `path` is not a string".to_string()) }
        };

        let op = match args.get("op") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => { return Err(format!("argument `op` is not a string: {:?}", v)) },
            None => { "".to_string() }
        };

        let todo = match args.get("todo") {
            Some(Value::String(s)) => s.clone(),
            Some(v) => { return Err(format!("argument `todo` is not a string: {:?}", v)) },
            None => { "".to_string() }
        };

        let mut chat_post = ChatPost {
            messages: ccx.messages.clone(),
            parameters: SamplingParameters {
                max_new_tokens: 300,
                temperature: Some(0.0),
                top_p: None,
                stop: vec![],
            },
            model: "gpt-3.5-turbo".to_string(),
            scratchpad: "".to_string(),
            stream: Some(false),
            temperature: Some(0.0),
            max_tokens: 300,
            tools: None,
            only_deterministic_messages: false,
            chat_id: "".to_string(),
        };

        let caps = crate::global_context::try_load_caps_quickly_if_not_present(ccx.global_context.clone(), 0).await.map_err(|e| {
            warn!("No caps: {:?}", e);
            format!("Network error communicating with the model (1)")
        })?;

        {
            let message_first: &mut ChatMessage = chat_post.messages.first_mut().unwrap();
            if message_first.role == "system" {
                message_first.content = PATCH_SYSTEM_PROMPT.to_string();
            } else {
                chat_post.messages.insert(0, ChatMessage {
                    role: "system".to_string(),
                    content: PATCH_SYSTEM_PROMPT.to_string(),
                    tool_calls: None,
                    tool_call_id: "".to_string(),
                });
            }
        }
        {
            let message_last: &mut ChatMessage = chat_post.messages.last_mut().unwrap();
            assert!(message_last.role == "assistant");
            assert!(message_last.tool_calls.is_some());
            message_last.tool_calls = None;
        }
        chat_post.messages.push(
            ChatMessage {
                role: "user".to_string(),
                content: format!("You are a diff generator. Use the format in the system prompt exactly. Your goal is the following:\n\n{}", todo),
                tool_calls: None,
                tool_call_id: "".to_string(),
            }
        );

        let (model_name, scratchpad_name, scratchpad_patch, n_ctx, _) = crate::http::routers::v1::chat::lookup_chat_scratchpad(caps.clone(), &chat_post).await?;
        let (client1, api_key) = {
            let cx_locked = ccx.global_context.write().await;
            (cx_locked.http_client.clone(), cx_locked.cmdline.api_key.clone())
        };
        let mut scratchpad = scratchpads::create_chat_scratchpad(
            ccx.global_context.clone(),
            caps,
            model_name.clone(),
            chat_post.clone(),
            &scratchpad_name,
            &scratchpad_patch,
            false,
            false,
        ).await?;
        let t1 = std::time::Instant::now();
        let prompt = scratchpad.prompt(
            n_ctx,
            &mut chat_post.parameters,
        ).await?;
        info!("diff prompt {:?}", t1.elapsed());
        let j = crate::restream::scratchpad_interaction_not_stream_json(
            ccx.global_context.clone(),
            scratchpad,
            "chat".to_string(),
            &prompt,
            model_name,
            client1,
            api_key,
            &chat_post.parameters,
            chat_post.only_deterministic_messages,
        ).await.map_err(|e| {
            warn!("Network error communicating with the (2): {:?}", e);
            format!("Network error communicating with the model (2)")
        })?;

        // Object {"choices": Array [Object {"finish_reason": String("stop"), "index": Number(0), "message": Object {"content": String("<<<<<<<< SEARCH\nimport sys, impotlib, os\n========\nimport sys, importlib, os\n>>>>>>>> REPLACE"), "role": String("assistant")}}], "created": Number(1718950188), "deterministic_messages": Array [], "id": String("chatcmpl-9cRky4cbgj3iftmSgtrxDd3J9vbHv"), "metering_balance": Number(-1958602), "metering_generated_tokens_n": Number(23), "metering_prompt_tokens_n": Number(437), "model": String("gpt-3.5-turbo-0125"), "object": String("chat.completion"), "pp1000t_generated": Number(1500), "pp1000t_prompt": Number(500), "system_fingerprint": Null, "usage": Object {"completion_tokens": Number(23), "prompt_tokens": Number(437), "total_tokens": Number(460)}}
        let choices_array = match j["choices"].as_array() {
            Some(array) => array,
            None => return Err("Unable to get choices array from JSON".to_string()),
        };

        let choice0 = match choices_array.get(0) {
            Some(Value::Object(o)) => o,
            Some(v) => { return Err(format!("choice[0] is not a dict: {:?}", v)) },
            None => { return Err("choice[0] doesn't exist".to_string()) }
        };

        let choice0_message = match choice0.get("message") {
            Some(Value::Object(o)) => o,
            Some(v) => { return Err(format!("choice[0].message is not a dict: {:?}", v)) },
            None => { return Err("choice[0].message doesn't exist".to_string()) }
        };

        let choice0_message_content = match choice0_message.get("content") {
            Some(Value::String(s)) => s,
            Some(v) => { return Err(format!("choice[0].message.content is not a string: {:?}", v)) },
            None => { return Err("choice[0].message.content doesn't exist".to_string()) }
        };

        let mut results = vec![];
        results.push(ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: format!("{}", choice0_message_content),
            tool_calls: None,
            tool_call_id: tool_call_id.clone(),
        }));
        Ok(results)
    }
}
