use std::sync::Arc;
use std::sync::RwLock as StdRwLock;

use async_trait::async_trait;
use serde_json::Value;
use tokenizers::Tokenizer;
use tokio::sync::RwLock as ARwLock;
use tracing::{error, info};

use crate::call_validation::{ChatMessage, ChatPost, ContextFile, SamplingParameters};
use crate::global_context::GlobalContext;
use crate::scratchpad_abstract::HasTokenizerAndEot;
use crate::scratchpad_abstract::ScratchpadAbstract;
use crate::scratchpads::chat_utils_limit_history::limit_messages_history;
use crate::scratchpads::chat_utils_rag::{run_at_commands, HasRagResults};

const DEBUG: bool = true;


pub struct DeltaSender {
    pub role_sent: String,
}

impl DeltaSender {
    pub fn new() -> Self {
        DeltaSender {
            role_sent: "".to_string(),
        }
    }

    pub fn feed_delta(&mut self, role: &str, delta: &str, finish_reason: &str) -> serde_json::Value {
        let x = serde_json::json!([{
            "index": 0,
            "delta": {
                "role": if role != self.role_sent.as_str() { serde_json::Value::String(role.to_string()) } else { serde_json::Value::Null },
                "content": delta
            },
            "finish_reason": if finish_reason == "" { serde_json::Value::Null } else { serde_json::Value::String(finish_reason.to_string()) }
        }]);
        self.role_sent = role.to_string();
        x
    }
}


// #[derive(Debug)]
pub struct ChatPassthrough {
    pub t: HasTokenizerAndEot,
    pub post: ChatPost,
    pub default_system_message: String,
    pub has_rag_results: HasRagResults,
    pub delta_sender: DeltaSender,
    pub global_context: Arc<ARwLock<GlobalContext>>,
    pub response_style: Option<String>,
}

impl ChatPassthrough {
    pub fn new(
        tokenizer: Arc<StdRwLock<Tokenizer>>,
        post: ChatPost,
        global_context: Arc<ARwLock<GlobalContext>>,
        response_style: Option<String>
    ) -> Self {
        ChatPassthrough {
            t: HasTokenizerAndEot::new(tokenizer),
            post,
            default_system_message: "".to_string(),
            has_rag_results: HasRagResults::new(),
            delta_sender: DeltaSender::new(),
            global_context,
            response_style,
        }
    }
}

#[async_trait]
impl ScratchpadAbstract for ChatPassthrough {
    fn apply_model_adaptation_patch(
        &mut self,
        patch: &serde_json::Value,
    ) -> Result<(), String> {
        self.default_system_message = patch.get("default_system_message").and_then(|x| x.as_str()).unwrap_or("").to_string();
        Ok(())
    }

    async fn prompt(
        &mut self,
        context_size: usize,
        sampling_parameters_to_patch: &mut SamplingParameters,
    ) -> Result<String, String> {
        info!("chat passthrough {} messages at start", &self.post.messages.len());
        let top_n: usize = 10;
        let last_user_msg_starts = run_at_commands(self.global_context.clone(), self.t.tokenizer.clone(), sampling_parameters_to_patch.max_new_tokens, context_size, &mut self.post, top_n, &mut self.has_rag_results).await;
        let limited_msgs: Vec<ChatMessage> = match limit_messages_history(&self.t, &self.post.messages, last_user_msg_starts, sampling_parameters_to_patch.max_new_tokens, context_size, &self.default_system_message) {
            Ok(res) => res,
            Err(e) => {
                error!("error limiting messages: {}", e);
                vec![]
            }
        };
        info!("chat passthrough {} messages -> {} messages after applying at-commands and limits, possibly adding the default system message", &self.post.messages.len(), &limited_msgs.len());
        let mut filtered_msgs: Vec<ChatMessage> = Vec::<ChatMessage>::new();
        for msg in &limited_msgs {
            if msg.role == "assistant" || msg.role == "system" || msg.role == "user" {
                filtered_msgs.push(msg.clone());
            } else if msg.role == "context_file" {
                match serde_json::from_str(&msg.content) {
                    Ok(res) => {
                        let vector_of_context_files: Vec<ContextFile> = res;
                        for context_file in &vector_of_context_files {
                            filtered_msgs.push(ChatMessage {
                                role: "user".to_string(),
                                content: format!("{}:{}-{}\n```\n{}```",
                                    context_file.file_name,
                                    context_file.line1,
                                    context_file.line2,
                                    context_file.file_content),
                            });
                        }
                    },
                    Err(e) => { error!("error parsing context file: {}", e); }
                }
            }
        }
        let prompt = "PASSTHROUGH ".to_string() + &serde_json::to_string(&filtered_msgs).unwrap();
        if DEBUG {
            for msg in &filtered_msgs {
                info!("filtered role={} {:?}", msg.role, crate::nicer_logs::first_n_chars(&msg.content, 30));
            }
        }
        Ok(prompt.to_string())
    }

    fn response_n_choices(  // old-school OpenAI
        &mut self,
        _choices: Vec<String>,
        _stopped: Vec<bool>,
    ) -> Result<serde_json::Value, String> {
        todo!();
        // detect if tool use or not
        // for choice in choices.iter() {
        //     let tool = true;
        //     if !tool {
        //         return serde_json::json!([choice]);
        //     } else {
        //         for tool_json in tools.iter() {
        //             // look up the tool
        //             t_real.execute(tool_json);
        //         }
        //         // postprocessing
        //     }
        // }
        // return serde_json::json!([]);
    }

    fn response_streaming(
        &mut self,
        delta: String,
        stop_toks: bool,
        stop_length: bool,
    ) -> Result<(serde_json::Value, bool), String> {
        // ChatCompletionChunk(id='chatcmpl-9PQr82sRGEXp7YaMUfK7OZlNOPYuF', choices=[Choice(delta=ChoiceDelta(content=None, function_call=None, role='assistant', tool_calls=[ChoiceDeltaToolCall(index=0, id='call_coiieM6pksUjrvo4qfLUdEFy', function=ChoiceDeltaToolCallFunction(arguments='', name='definition'), type='function')]), finish_reason=None, index=0, logprobs=None)], created=1715848462, model='gpt-3.5-turbo-0125', object='chat.completion.chunk', system_fingerprint=None)
        // ChatCompletionChunk(id='chatcmpl-9PQr82sRGEXp7YaMUfK7OZlNOPYuF', choices=[Choice(delta=ChoiceDelta(content=None, function_call=None, role=None, tool_calls=[ChoiceDeltaToolCall(index=0, id=None, function=ChoiceDeltaToolCallFunction(arguments='{"', name=None), type=None)]), finish_reason=None, index=0, logprobs=None)], created=1715848462, model='gpt-3.5-turbo-0125', object='chat.completion.chunk', system_fingerprint=None)
        // ChatCompletionChunk(id='chatcmpl-9PQr82sRGEXp7YaMUfK7OZlNOPYuF', choices=[Choice(delta=ChoiceDelta(content=None, function_call=None, role=None, tool_calls=[ChoiceDeltaToolCall(index=0, id=None, function=ChoiceDeltaToolCallFunction(arguments='symbol', name=None), type=None)]), finish_reason=None, index=0, logprobs=None)], created=1715848462, model='gpt-3.5-turbo-0125', object='chat.completion.chunk', system_fingerprint=None)
        // ChatCompletionChunk(id='chatcmpl-9PQr82sRGEXp7YaMUfK7OZlNOPYuF', choices=[Choice(delta=ChoiceDelta(content=None, function_call=None, role=None, tool_calls=[ChoiceDeltaToolCall(index=0, id=None, function=ChoiceDeltaToolCallFunction(arguments='":"', name=None), type=None)]), finish_reason=None, index=0, logprobs=None)], created=1715848462, model='gpt-3.5-turbo-0125', object='chat.completion.chunk', system_fingerprint=None)
        // ChatCompletionChunk(id='chatcmpl-9PQr82sRGEXp7YaMUfK7OZlNOPYuF', choices=[Choice(delta=ChoiceDelta(content=None, function_call=None, role=None, tool_calls=[ChoiceDeltaToolCall(index=0, id=None, function=ChoiceDeltaToolCallFunction(arguments='frog', name=None), type=None)]), finish_reason=None, index=0, logprobs=None)], created=1715848462, model='gpt-3.5-turbo-0125', object='chat.completion.chunk', system_fingerprint=None)
        // ChatCompletionChunk(id='chatcmpl-9PQr82sRGEXp7YaMUfK7OZlNOPYuF', choices=[Choice(delta=ChoiceDelta(content=None, function_call=None, role=None, tool_calls=[ChoiceDeltaToolCall(index=0, id=None, function=ChoiceDeltaToolCallFunction(arguments='.F', name=None), type=None)]), finish_reason=None, index=0, logprobs=None)], created=1715848462, model='gpt-3.5-turbo-0125', object='chat.completion.chunk', system_fingerprint=None)
        // ChatCompletionChunk(id='chatcmpl-9PQr82sRGEXp7YaMUfK7OZlNOPYuF', choices=[Choice(delta=ChoiceDelta(content=None, function_call=None, role=None, tool_calls=[ChoiceDeltaToolCall(index=0, id=None, function=ChoiceDeltaToolCallFunction(arguments='rog', name=None), type=None)]), finish_reason=None, index=0, logprobs=None)], created=1715848462, model='gpt-3.5-turbo-0125', object='chat.completion.chunk', system_fingerprint=None)
        // ChatCompletionChunk(id='chatcmpl-9PQr82sRGEXp7YaMUfK7OZlNOPYuF', choices=[Choice(delta=ChoiceDelta(content=None, function_call=None, role=None, tool_calls=[ChoiceDeltaToolCall(index=0, id=None, function=ChoiceDeltaToolCallFunction(arguments='"}', name=None), type=None)]), finish_reason=None, index=0, logprobs=None)], created=1715848462, model='gpt-3.5-turbo-0125', object='chat.completion.chunk', system_fingerprint=None)
        // ChatCompletionChunk(id='chatcmpl-9PQr82sRGEXp7YaMUfK7OZlNOPYuF', choices=[Choice(delta=ChoiceDelta(content=None, function_call=None, role=None, tool_calls=None), finish_reason='tool_calls', index=0, logprobs=None)], created=1715848462, model='gpt-3.5-turbo-0125', object='chat.completion.chunk', system_fingerprint=None)
        // info!("chat passthrough response_streaming delta={:?}, stop_toks={}, stop_length={}", delta, stop_toks, stop_length);
        let finished = stop_toks || stop_length;
        let finish_reason = if finished {
            if stop_toks { "stop".to_string() } else { "length".to_string() }
        } else {
            "".to_string()
        };
        let json_choices = self.delta_sender.feed_delta("assistant", &delta, &finish_reason);
        let ans = serde_json::json!({
            "choices": json_choices,
            "object": "chat.completion.chunk",
        });
        Ok((ans, finished))
    }

    fn response_spontaneous(&mut self) -> Result<Vec<Value>, String>  {
        return self.has_rag_results.response_streaming();
    }
    fn response_style(&self) -> Option<String> {
        self.response_style.clone()
    }
}
