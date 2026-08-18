"""Anthropic-format backend that records every request verbatim.

The router forwards Anthropic-dialect providers untouched apart from the
model name, so what lands here is exactly what Claude Code sent — which is
the point: it makes a /compact request readable next to an ordinary turn.

Requests are appended to captured.jsonl, one JSON object per line: the whole
body under "body", plus a short digest beside it for skimming.
"""

import datetime
import http.server
import json
import pathlib

HERE = pathlib.Path(__file__).resolve().parent
LOG = HERE / "captured.jsonl"


def text_of(message):
    content = message.get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return " ".join(p.get("text", "") for p in content if isinstance(p, dict))
    return ""


def digest(body):
    messages = body.get("messages") or []
    system = body.get("system")
    if isinstance(system, list):
        system = " || ".join(b.get("text", "") for b in system if isinstance(b, dict))
    return {
        "model": body.get("model"),
        "max_tokens": body.get("max_tokens"),
        "stream": bool(body.get("stream")),
        "n_messages": len(messages),
        "n_tools": len(body.get("tools") or []),
        "system_chars": len(system or ""),
        "system_head": (system or "")[:200],
        "roles": [m.get("role") for m in messages][-6:],
        "last_text_tail": text_of(messages[-1])[-600:] if messages else "",
        "keys": sorted(body.keys()),
    }


def sse(events):
    return "".join(
        f"event: {name}\ndata: {json.dumps(data)}\n\n" for name, data in events
    ).encode()


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        raw = self.rfile.read(int(self.headers.get("content-length", 0)))
        try:
            body = json.loads(raw)
        except json.JSONDecodeError:
            body = {"unparsed": raw.decode("utf-8", "replace")}

        with LOG.open("a") as f:
            f.write(
                json.dumps(
                    {
                        "at": datetime.datetime.now().isoformat(timespec="seconds"),
                        "path": self.path,
                        "digest": digest(body),
                        "body": body,
                    }
                )
                + "\n"
            )

        reply = "captured"
        model = body.get("model", "test")
        if body.get("stream"):
            self.send_response(200)
            self.send_header("content-type", "text/event-stream")
            self.end_headers()
            self.wfile.write(
                sse(
                    [
                        ("message_start", {"type": "message_start", "message": {
                            "id": "msg_capture", "type": "message", "role": "assistant",
                            "model": model, "content": [], "stop_reason": None,
                            "stop_sequence": None,
                            "usage": {"input_tokens": 1, "output_tokens": 1}}}),
                        ("content_block_start", {"type": "content_block_start", "index": 0,
                                                 "content_block": {"type": "text", "text": ""}}),
                        ("content_block_delta", {"type": "content_block_delta", "index": 0,
                                                 "delta": {"type": "text_delta", "text": reply}}),
                        ("content_block_stop", {"type": "content_block_stop", "index": 0}),
                        ("message_delta", {"type": "message_delta",
                                           "delta": {"stop_reason": "end_turn",
                                                     "stop_sequence": None},
                                           "usage": {"output_tokens": 1}}),
                        ("message_stop", {"type": "message_stop"}),
                    ]
                )
            )
            return

        payload = {
            "id": "msg_capture",
            "type": "message",
            "role": "assistant",
            "model": model,
            "content": [{"type": "text", "text": reply}],
            "stop_reason": "end_turn",
            "stop_sequence": None,
            "usage": {"input_tokens": 1, "output_tokens": 1},
        }
        encoded = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    print(f"capturing to {LOG}")
    http.server.HTTPServer(("127.0.0.1", 8131), Handler).serve_forever()
