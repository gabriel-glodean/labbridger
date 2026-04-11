"""
Minimal mock Ollama server that streams NDJSON responses, identical
in shape to the real Ollama API.  Used for testing the streaming relay.

Supported endpoints:
  GET  /api/tags              – list "models"
  POST /api/generate          – stream a fake generate response
  POST /api/chat              – stream a fake chat response
  POST /api/show              – return fake model info
"""
import json
import time
import datetime
from flask import Flask, Response, request, jsonify

app = Flask(__name__)

WORDS = (
    "The quick brown fox jumps over the lazy dog. "
    "Streaming works perfectly through the relay!"
).split()


def _now() -> str:
    return datetime.datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%S.%f") + "Z"


# ── /api/tags ─────────────────────────────────────────────────────────────────

@app.route("/api/tags", methods=["GET"])
def tags():
    return jsonify({
        "models": [
            {
                "name": "mock:latest",
                "modified_at": _now(),
                "size": 1_234_567,
            }
        ]
    })


# ── /api/generate ─────────────────────────────────────────────────────────────

@app.route("/api/generate", methods=["POST"])
def generate():
    data = request.get_json(force=True, silent=True) or {}
    prompt  = data.get("prompt", "hello")
    model   = data.get("model", "mock")
    do_stream = data.get("stream", True)
    delay   = float(data.get("_mock_delay", 0.08))   # undocumented test knob

    reply_words = (f"[mock/generate] prompt='{prompt}' → ").split() + WORDS

    def stream_gen():
        for w in reply_words:
            chunk = {
                "model": model,
                "created_at": _now(),
                "response": w + " ",
                "done": False,
            }
            yield json.dumps(chunk) + "\n"
            time.sleep(delay)

        yield json.dumps({
            "model": model,
            "created_at": _now(),
            "response": "",
            "done": True,
            "total_duration": int(len(reply_words) * delay * 1e9),
        }) + "\n"

    if do_stream:
        return Response(stream_gen(), mimetype="application/x-ndjson")

    # non-streaming: collect full text first
    full = "".join(w + " " for w in reply_words)
    return jsonify({
        "model": model,
        "created_at": _now(),
        "response": full.strip(),
        "done": True,
    })


# ── /api/chat ─────────────────────────────────────────────────────────────────

@app.route("/api/chat", methods=["POST"])
def chat():
    data = request.get_json(force=True, silent=True) or {}
    messages  = data.get("messages", [])
    model     = data.get("model", "mock")
    do_stream = data.get("stream", True)
    delay     = float(data.get("_mock_delay", 0.08))

    last_content = messages[-1]["content"] if messages else "hello"
    reply_words = (f"[mock/chat] you said: '{last_content}' → ").split() + WORDS

    def stream_chat():
        for w in reply_words:
            chunk = {
                "model": model,
                "created_at": _now(),
                "message": {"role": "assistant", "content": w + " "},
                "done": False,
            }
            yield json.dumps(chunk) + "\n"
            time.sleep(delay)

        yield json.dumps({
            "model": model,
            "created_at": _now(),
            "message": {"role": "assistant", "content": ""},
            "done": True,
        }) + "\n"

    if do_stream:
        return Response(stream_chat(), mimetype="application/x-ndjson")

    full = "".join(w + " " for w in reply_words)
    return jsonify({
        "model": model,
        "created_at": _now(),
        "message": {"role": "assistant", "content": full.strip()},
        "done": True,
    })


# ── /api/show ─────────────────────────────────────────────────────────────────

@app.route("/api/show", methods=["POST"])
def show():
    data = request.get_json(force=True, silent=True) or {}
    return jsonify({
        "license": "mock",
        "modelfile": "FROM mock",
        "parameters": "",
        "template": "{{ .Prompt }}",
        "details": {"family": "mock", "parameter_size": "1B", "quantization_level": "Q4_0"},
    })


if __name__ == "__main__":
    print("Mock Ollama listening on :11434", flush=True)
    # use_reloader=False keeps the single-threaded streamer predictable
    app.run(host="0.0.0.0", port=11434, threaded=True, use_reloader=False)

