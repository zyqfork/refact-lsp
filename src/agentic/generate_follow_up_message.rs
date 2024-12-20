use std::sync::Arc;
use tokio::sync::{RwLock as ARwLock, Mutex as AMutex};
use serde_json::Value;

use crate::global_context::GlobalContext;
use crate::at_commands::at_commands::AtCommandsContext;
use crate::subchat::subchat_single;
use crate::call_validation::ChatMessage;

pub async fn generate_follow_up_message(
    mut messages: Vec<ChatMessage>,
    gcx: Arc<ARwLock<GlobalContext>>,
    model_name: &str,
    chat_id: &str,
) -> Result<Vec<String>, String> {
    let last_assistant_msg_text;
    if let Some(last_assistant_msg) = messages.iter().rev().find(|m| m.role == "assistant").cloned() {
        // messages.clear();
        // messages.push(last_assistant_msg);
        last_assistant_msg_text = last_assistant_msg.content.content_text_only();
    } else {
        return Err(format!("The last message is not role=assistant"));
    }

    // If the robot message is an open question, return empty list.

    messages = vec![
        ChatMessage::new(
            "system".to_string(),
            concat!(
                "Super simple job today, generate follow-ups! In the first message you will receive a question or statement generated by a robot.\n",
                "Generate up to 3 most likely short follow-ups by the user to the robot message, in 3 words or less, like 'Go ahead' 'Looks fantastic!' 'Never mind' etc.\n",
                "Put first the option that allows robot to continue.\n",
                "All the follow-ups must mean different things, not 3 ways to say \"yes\".\n",
                "If there are no simple answers possible, return empty list. If the is no question, return an empty list.\n",
                "\n",
                "Output must be this simple json:\n",
                "\n",
                "[\"Follow up 1\", \"Follow up 2\"]\n",
                "\n",
                "Don't write backquotes, just this format.\n",
            ).to_string(),
        ),
        ChatMessage::new(
            "user".to_string(),
            last_assistant_msg_text,
        ),
    ];

    let ccx = Arc::new(AMutex::new(AtCommandsContext::new(
        gcx.clone(),
        8000,
        1,
        false,
        messages.clone(),
        chat_id.to_string(),
        false,
    ).await));
    let updated_messages: Vec<Vec<ChatMessage>> = subchat_single(
        ccx.clone(),
        model_name,
        messages.clone(),
        vec![],
        None,
        false,
        Some(0.5),
        None,
        1,
        None,
        None,
        None,
    ).await?;
    let response = updated_messages.into_iter().next().map(|x| x.into_iter().last().map(|last_m| {
        last_m.content.content_text_only() })).flatten().ok_or("No commit message found".to_string())?;

    tracing::info!("follow-up model says1 {:?}", messages);
    tracing::info!("follow-up model says2 {:?}", response);

    let parsed_response: Value = serde_json::from_str(&response).map_err(|e| e.to_string())?;
    let follow_ups = parsed_response.as_array()
        .ok_or("Invalid JSON format")?
        .iter()
        .map(|v| v.as_str().unwrap_or("").to_string())
        .collect();

    Ok(follow_ups)
}