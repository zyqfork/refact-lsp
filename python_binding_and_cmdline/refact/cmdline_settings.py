import os
import yaml
from pydantic import BaseModel
from pydantic import BaseModel, ValidationError
from typing import Optional, Dict, List
import aiohttp


class CapsModel(BaseModel):
    n_ctx: int
    similar_models: List[str]
    supports_tools: bool


class Caps(BaseModel):
    cloud_name: str
    code_chat_models: Dict[str, CapsModel]
    code_chat_default_model: str
    embedding_model: str


class SettingsCLI(BaseModel):
    address_url: str
    api_key: str
    insecure_ssl: bool = False
    ast: bool = True
    ast_file_limit: int = 20000
    vecdb: bool = True
    vecdb_file_limit: int = 5000
    experimental: bool = False
    basic_telemetry: bool = True


class CmdlineSettings:
    def __init__(self, caps: Caps, args):
        self.caps = caps
        self.model = args.model or caps.code_chat_default_model
        self.project_path = args.path_to_project

    def n_ctx(self):
        return self.caps.code_chat_models[self.model].n_ctx


args: Optional[CmdlineSettings] = None
cli_yaml: Optional[SettingsCLI] = None


async def fetch_caps(base_url: str) -> Caps:
    url = f"{base_url}/caps"
    async with aiohttp.ClientSession() as session:
        async with session.get(url) as response:
            if response.status == 200:
                data = await response.json()
                return Caps(**data)  # Parse the JSON data into the Caps model
            else:
                print(f"cannot fetch {url}\n{response.status}")
                return None


def load_cli_or_auto_configure():
    cli_yaml_path = os.path.expanduser("~/.cache/refact/cli.yaml")
    if not os.path.exists(cli_yaml_path):
        # No config, autodetect
        print("First run. Welcome, I'll try to set up a reasonable config.")
    with open(cli_yaml_path, 'r') as file:
        data = yaml.safe_load(file)
        return SettingsCLI.model_validate(data)
