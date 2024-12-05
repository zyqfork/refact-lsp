use std::sync::Arc;
use std::collections::HashMap;
use tokio::sync::Mutex as AMutex;
use tokio::process::Command;
use async_trait::async_trait;
use tracing::{error, info};
use serde::{Deserialize, Serialize};

use crate::at_commands::at_commands::AtCommandsContext;
use crate::call_validation::{ContextEnum, ChatMessage, ChatContent};

use crate::tools::tools_description::Tool;
use serde_json::Value;
use crate::integrations::integr_abstract::Integration;


#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[allow(non_snake_case)]
pub struct IntegrationGitHub {
    pub gh_binary_path: Option<String>,
    pub GH_TOKEN: String,
}

#[derive(Default)]
pub struct ToolGithub {
    pub integration_github: IntegrationGitHub,
}

impl Integration for ToolGithub {
    fn integr_settings_apply(&mut self, value: &Value) -> Result<(), String> {
        let integration_github = serde_json::from_value::<IntegrationGitHub>(value.clone())
            .map_err(|e|e.to_string())?;
        self.integration_github = integration_github;
        Ok(())
    }

    fn integr_yaml2json(&self, value: &serde_yaml::Value) -> Result<Value, String> {
        let integration_github = serde_yaml::from_value::<IntegrationGitHub>(value.clone()).map_err(|e| {
            let location = e.location().map(|loc| format!(" at line {}, column {}", loc.line(), loc.column())).unwrap_or_default();
            format!("{}{}", e.to_string(), location)
        })?;
        serde_json::to_value(&integration_github).map_err(|e| e.to_string())
    }

    fn integr_upgrade_to_tool(&self, integr_name: &String) -> Box<dyn Tool + Send> {
        Box::new(ToolGithub {integration_github: self.integration_github.clone()}) as Box<dyn Tool + Send>
    }

    fn integr_settings_as_json(&self) -> Result<Value, String> {
        serde_json::to_value(&self.integration_github).map_err(|e| e.to_string())
    }

    fn integr_settings_default(&self) -> String { DEFAULT_GITHUB_INTEGRATION_YAML.to_string() }
    fn icon_link(&self) -> String { "https://cdn-icons-png.flaticon.com/512/25/25231.png".to_string() }
}

#[async_trait]
impl Tool for ToolGithub {
    fn as_any(&self) -> &dyn std::any::Any { self }

    async fn tool_execute(
        &mut self,
        _ccx: Arc<AMutex<AtCommandsContext>>,
        tool_call_id: &String,
        args: &HashMap<String, Value>,
    ) -> Result<(bool, Vec<ContextEnum>), String> {
        let project_dir = match args.get("project_dir") {
            Some(Value::String(s)) => s,
            Some(v) => return Err(format!("argument `project_dir` is not a string: {:?}", v)),
            None => return Err("Missing argument `project_dir`".to_string())
        };
        let command_args = parse_command_args(args)?;

        let gh_command = self.integration_github.gh_binary_path.as_deref().unwrap_or("gh");
        let output = Command::new(gh_command)
            .args(&command_args)
            .current_dir(&project_dir)
            .env("GH_TOKEN", &self.integration_github.GH_TOKEN)
            .output()
            .await
            .map_err(|e| e.to_string())?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !stderr.is_empty() {
            error!("Error: {:?}", stderr);
            return Err(stderr);
        }

        let content = if stdout.starts_with("[") {
            match serde_json::from_str::<Value>(&stdout) {
                Ok(Value::Array(arr)) => {
                    let row_count = arr.len();
                    format!("{}\n\n💿 The UI has the capability to view tool result json efficiently. The result contains {} rows. Write no more than 3 rows as text and possibly \"and N more\" wording, keep it short.",
                        stdout, row_count
                    )
                },
                Ok(_) => stdout,
                Err(_) => stdout,
            }
        } else {
            stdout
        };
        let mut results = vec![];
        results.push(ContextEnum::ChatMessage(ChatMessage {
            role: "tool".to_string(),
            content: ChatContent::SimpleText(content),
            tool_calls: None,
            tool_call_id: tool_call_id.clone(),
            ..Default::default()
        }));

        Ok((false, results))
    }

    fn command_to_match_against_confirm_deny(
        &self,
        args: &HashMap<String, Value>,
    ) -> Result<String, String> {
        let mut command_args = parse_command_args(args)?;
        command_args.insert(0, "gh".to_string());
        Ok(command_args.join(" "))
    }
}

fn parse_command_args(args: &HashMap<String, Value>) -> Result<Vec<String>, String> {
    let command = match args.get("command") {
        Some(Value::String(s)) => s,
        Some(v) => return Err(format!("argument `command` is not a string: {:?}", v)),
        None => return Err("Missing argument `command`".to_string())
    };

    let mut parsed_args = shell_words::split(&command).map_err(|e| e.to_string())?;
    if parsed_args.is_empty() {
        return Err("Parsed command is empty".to_string());
    }
    for (i, arg) in parsed_args.iter().enumerate() {
        info!("argument[{}]: {}", i, arg);
    }
    if parsed_args[0] == "gh" {
        parsed_args.remove(0);
    }

    Ok(parsed_args)
}

const DEFAULT_GITHUB_INTEGRATION_YAML: &str = r#"
# GitHub integration

# GH_TOKEN: "GH_xxx"                      # To get a token, check out https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens
# gh_binary_path: "/opt/homebrew/bin/gh"  # Uncomment to set a custom path for the gh binary, defaults to "gh"
"#;
