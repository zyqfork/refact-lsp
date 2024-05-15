use serde::{Deserialize, Serialize};
use serde_json::{json, Result, Value};


pub const AT_COMMANDS_DICT: &str = r####"
[
    {
        "name": "@workspace",
        "description": "Using given query, find all pieces of code in the project by vectorizing query and finding all similar pieces of code by comparing their cosien distances.",
        "parameters": ["query"],
        "parameters_required": ["query"]
    },
    {
        "name": "@file",
        "description": "Read the file located using given file_path and provide its content",
        "parameters": ["file_path"],
        "parameters_required": ["file_path"]
    },
    {
        "name": "@definition",
        "description": "Find definition of a symbol in a project using AST. Symbol could be: function, method, class, type alias.",
        "parameters": ["symbol"],
        "parameters_required": ["symbol"]
    },
    {
        "name": "@references",
        "description": "Find usages of a symbol in a project using AST. Symbol could be: function, method, class, type alias.",
        "parameters": ["symbol"],
        "parameters_required": ["symbol"]
    },
    {
        "name": "@symbols-at",
        "description": "Using a file_path in a following format: file_name.ext:line_number, find all symbols at the given line number of the file.",
        "parameters": ["file_path"],
        "parameters_required": ["file_path"]
    }
}
"####;

pub const AT_COMMANDS_PARAMS: &str = r####"
[
    {
        "name": "query",
        "type": "string",
        "description": "Short text written in natural language. Single line or a paragraph. Query will be vectorized and used to find similar pieces of code in the project."
    },
    {
        "name": "file_path",
        "type": "string",
        "description": "absolute path to the file or filename to be found within the project."
    },
    {
        "name": "symbol",
        "type": "string",
        "description": "The name of the symbol (function, method, class, type alias) to find within the project."
    },
]
"####;


#[derive(Deserialize)]
struct AtCommandDictDeserialize{
    name: String,
    description: String,
    parameters: Vec<String>,
    parameters_required: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct AtCommandDict {
    pub name: String,
    pub description: String,
    pub parameters: Vec<AtParamDict>,
    pub parameters_required: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct AtParamDict {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
    pub description: String,
}

impl AtCommandDict {
    pub fn new(cmd: &AtCommandDictDeserialize, params: &Vec<AtParamDict>) -> Self {
        AtCommandDict {
            name: cmd.name.clone(),
            description: cmd.description.clone(),
            parameters: cmd.parameters.iter()
                .map(
                    |name| params.iter()
                        .find(|param| &param.name == name).unwrap()
                )
                .cloned().collect(),
            parameters_required: cmd.parameters_required.clone(),
        }
    }
    pub fn into_openai_style(self) -> Value {
        let params_properties = self.parameters.iter().map(|param| {
            (
                param.name.clone(),
                json!({
                "type": param.param_type,
                "description": param.description
            })
            )
        }).collect::<serde_json::Map<_, _>>();

        let function_json = json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": {
                    "type": "object",
                    "properties": params_properties,
                    "required": self.parameters_required
                }
            }
        });
        function_json
    }
}

pub fn at_commands_dicts() -> Result<Vec<AtCommandDict>> {
    let commands_dict_raw: Vec<AtCommandDictDeserialize> = serde_json::from_str(AT_COMMANDS_DICT)?;
    let params_dict_raw: Vec<AtParamDict> = serde_json::from_str(AT_COMMANDS_PARAMS)?;
    
    Ok(commands_dict_raw.iter().map(|cmd| AtCommandDict::new(cmd, &params_dict_raw)).collect::<Vec<_>>())
}
