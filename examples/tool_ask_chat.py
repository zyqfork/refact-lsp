import requests


code_in_question = """
if __name__ == "__main__":
    class Toad(frog.Frog):
        def __init__(self, x, y, vx, vy):
            super().__init__(x, y, vx, vy)
            self.name = "Bob"
    # toad = EuropeanCommonToad(100, 100, 200, -200)
    # toad.jump(W, H)
    # print(toad.name, toad.x, toad.y)
"""


def ask_chat():
    response = requests.post(
        "http://127.0.0.1:8001/v1/chat",
        json={
            "messages": messages,
            "temperature": 0.6,
            "max_tokens": 300,
            "model": "gpt-3.5-turbo",
            "tool_use": True
        },
        timeout=60,
    )
    assert response.status_code == 200
    return response.text


messages = [
    ["system", "You are a coding assistant. Use your sense of humor. Before answering, use tool calls to fetch definitions of all the types and functions. Your first answer must consist of tool calls."],
    ["user", "Explain what that code does\n```%s```" % code_in_question],
]
if __name__ == "__main__":
    print(ask_chat())

