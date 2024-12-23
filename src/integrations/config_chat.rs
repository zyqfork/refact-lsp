use std::sync::Arc;
use std::fs;
use tokio::sync::RwLock as ARwLock;

use crate::global_context::GlobalContext;
use crate::call_validation::{ChatContent, ChatMessage, ContextFile, ChatMeta};
use crate::scratchpads::scratchpad_utils::HasRagResults;
use crate::integrations::yaml_schema::ISchema;


pub async fn mix_config_messages(
    gcx: Arc<ARwLock<GlobalContext>>,
    chat_meta: &ChatMeta,
    messages: &mut Vec<ChatMessage>,
    stream_back_to_user: &mut HasRagResults,
) {
    assert!(messages[0].role != "system");  // we are here to add this, can't already exist
    tracing::info!("post.integr_config_path {:?}", chat_meta.current_config_file);

    let mut context_file_vec = Vec::new();
    let all_integrations = crate::integrations::setting_up_integrations::integrations_all(gcx.clone()).await;
    for ig in all_integrations.integrations {
        if !ig.integr_config_exists {
            continue;
        }
        let file_content = match fs::read_to_string(&ig.integr_config_path) {
            Ok(content) => content,
            Err(err) => {
                tracing::error!("Failed to read file for integration {}: {:?}", ig.integr_config_path, err);
                continue;
            }
        };
        let context_file = ContextFile {
            file_name: ig.integr_config_path.clone(),
            file_content,
            line1: 0,
            line2: 0,
            symbols: vec![],
            gradient_type: -1,
            usefulness: 100.0,
        };
        context_file_vec.push(context_file);
    }

    let (config_dirs, global_config_dir) = crate::integrations::setting_up_integrations::get_config_dirs(gcx.clone()).await;
    let mut variables_yaml_instruction = String::new();
    for dir in config_dirs.iter().chain(std::iter::once(&global_config_dir)) {
        let variables_path = dir.join("variables.yaml");
        if variables_path.exists() {
            match fs::read_to_string(&variables_path) {
                Ok(file_content) => {
                    let context_file = ContextFile {
                        file_name: variables_path.to_string_lossy().to_string(),
                        file_content,
                        line1: 0,
                        line2: 0,
                        symbols: vec![],
                        gradient_type: -1,
                        usefulness: 100.0,
                    };
                    context_file_vec.push(context_file);
                }
                Err(err) => {
                    tracing::error!("Failed to read variables.yaml in dir {}: {:?}", dir.display(), err);
                }
            }
        } else {
            variables_yaml_instruction.push_str(format!("{}\n", variables_path.display()).as_str());
        }
    }

    let schema_message = match crate::integrations::setting_up_integrations::integration_config_get(
        chat_meta.current_config_file.clone(),
    ).await {
        Ok(the_get) => {
            let mut schema_struct: ISchema = serde_json::from_value(the_get.integr_schema).unwrap();   // will not fail because we have test_integration_schemas()
            schema_struct.docker = None;
            schema_struct.smartlinks.clear();
            tracing::info!("schema_struct {}", serde_json::to_string_pretty(&schema_struct).unwrap());
            tracing::info!("sample values {}", serde_json::to_string_pretty(&the_get.integr_values).unwrap());
            let mut msg = format!(
                "This is the data schema for the {}\n\n{}\n\n",
                chat_meta.current_config_file,
                serde_json::to_string(&schema_struct).unwrap(),
            );
            if the_get.integr_config_exists {
                msg.push_str(format!("This is how the system loads the YAML so you can detect which fields are not loaded in reality:\n\n{}\n\n", serde_json::to_string(&the_get.integr_values).unwrap()).as_str());
            } else {
                let mut yaml_value = serde_yaml::to_value(&the_get.integr_values).unwrap();
                if let serde_yaml::Value::Mapping(ref mut map) = yaml_value {
                    let mut available_map = serde_yaml::Mapping::new();
                    available_map.insert(serde_yaml::Value::String("on_your_laptop".to_string()), serde_yaml::Value::Bool(schema_struct.available.on_your_laptop_possible));
                    available_map.insert(serde_yaml::Value::String("when_isolated".to_string()), serde_yaml::Value::Bool(schema_struct.available.when_isolated_possible));
                    map.insert(serde_yaml::Value::String("available".to_string()), serde_yaml::Value::Mapping(available_map));
                }
                msg.push_str(format!("The file doesn't exist, so here is a sample YAML to give you an idea how this config might look in YAML:\n\n{}\n\n", serde_yaml::to_string(&yaml_value).unwrap()).as_str());
            }
            if !variables_yaml_instruction.is_empty() {
                msg.push_str(format!("Pay attention to variables.yaml files, you see the existing ones above, but also here are all the other paths they can potentially exist:\n{}\n\n", variables_yaml_instruction).as_str());
            }
            ChatMessage {
                role: "cd_instruction".to_string(),
                content: ChatContent::SimpleText(msg),
                tool_calls: None,
                tool_call_id: String::new(),
                usage: None,
            }
        },
        Err(e) => {
            tracing::error!("Failed to load integration {}: {}", chat_meta.current_config_file, e);
            return;
        }
    };

    let mut error_log = Vec::new();
    let custom = crate::yaml_configs::customization_loader::load_customization(gcx.clone(), true, &mut error_log).await;
    // XXX: let model know there are errors
    for e in error_log.iter() {
        tracing::error!(
            "{}:{} {:?}",
            crate::nicer_logs::last_n_chars(&e.integr_config_path, 30),
            e.error_line,
            e.error_msg,
        );
    }

    let sp: &crate::yaml_configs::customization_loader::SystemPrompt = custom.system_prompts.get("configurator").unwrap();

    let context_file_message = ChatMessage {
        role: "context_file".to_string(),
        content: ChatContent::SimpleText(serde_json::to_string(&context_file_vec).unwrap()),
        tool_calls: None,
        tool_call_id: String::new(),
        usage: None,
    };
    let system_message = ChatMessage {
        role: "system".to_string(),
        content: ChatContent::SimpleText(
            crate::scratchpads::chat_utils_prompts::system_prompt_add_workspace_info(gcx.clone(), &sp.text).await
        ),
        tool_calls: None,
        tool_call_id: String::new(),
        usage: None,
    };

    // Interestingly, here you can stream messages to user or not, and both options will work -- this function will be called or not called again the next chat call.
    if messages.len() == 1 {
        stream_back_to_user.push_in_json(serde_json::json!(system_message));
        stream_back_to_user.push_in_json(serde_json::json!(context_file_message));
        stream_back_to_user.push_in_json(serde_json::json!(schema_message));
    } else {
        tracing::error!("more than 1 message when mixing configurtion chat context, bad things might happen!");
    }

    messages.splice(0..0, vec![system_message, context_file_message, schema_message]);

    for msg in messages.iter_mut() {
        if let ChatContent::SimpleText(ref mut content) = msg.content {
            *content = content.replace("%CURRENT_CONFIG%", &chat_meta.current_config_file);
        }
    }
}
