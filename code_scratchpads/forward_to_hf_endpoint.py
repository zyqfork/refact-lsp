import textwrap
import aiohttp
import time
import os
import json
import logging
from typing import Dict, Any, Optional


logger = logging.getLogger("HF_CLIENT")


_reuse_session: Optional[aiohttp.ClientSession] = None


def global_hf_session_get():
    global _reuse_session
    if _reuse_session is None:
        _reuse_session = aiohttp.ClientSession()
        _reuse_session.headers.update({"Content-Type": "application/json"})
    return _reuse_session


def global_hf_session_close():
    global _reuse_session
    if _reuse_session is not None:
        _reuse_session.close()
    _reuse_session = None


async def real_work(
    model_name: str,
    prompt: str,
    sampling_parameters: Dict[str, Any],
    stream: bool,
    auth_from_client: Optional[str],
):
    session = global_hf_session_get()
    url = "https://api-inference.huggingface.co/models/" + model_name
    headers = {
        "Authorization": "Bearer " + (auth_from_client or os.environ["HUGGINGFACE_TOKEN"]),
    }
    data = {
        "inputs": prompt,
        "parameters": sampling_parameters,
        "stream": stream,
    }
    t0 = time.time()
    if stream:
        async with session.post(url, json=data, headers=headers) as response:
            async for byteline in response.content:
                # TODO: handle response errors
                txt = byteline.decode("utf-8").strip()
                if not txt.startswith("data:"):
                    continue
                txt = txt[5:]
                # print("-"*20, "line", "-"*20, "%0.2fms" % ((time.time() - t0) * 1000))
                # print(txt)
                # print("-"*20, "/line", "-"*20)
                line = json.loads(txt)
                yield line
    else:
        async with session.post(url, json=data) as response:
            response_txt = await response.text()
            if response.status == 200:
                response_json = json.loads(response_txt)
                yield response_json
            else:
                logger.warning("forward_to_hf_endpoint: http status %s, response text was:\n%s" % (response.status, response_txt))
                raise ValueError(json.dumps({"error": "hf_endpoint says: %s" % (textwrap.shorten(response_txt, 50))}))
