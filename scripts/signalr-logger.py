"""mitmproxy addon to log SignalR WebSocket messages."""
import json

class SignalRLogger:
    def websocket_message(self, flow):
        msg = flow.websocket.messages[-1]
        direction = ">>>" if msg.from_client else "<<<"
        content = msg.content
        if isinstance(content, bytes):
            content = content.decode(errors="replace")

        # SignalR messages are delimited by \x1e
        for part in content.split("\x1e"):
            part = part.strip()
            if not part:
                continue
            try:
                parsed = json.loads(part)
                target = parsed.get("target", "")
                msg_type = parsed.get("type", 0)
                inv_id = parsed.get("invocationId", "")
                args = parsed.get("arguments", [])
                result = parsed.get("result", None)
                error = parsed.get("error", None)

                if msg_type == 1:  # Invocation
                    print(f"{direction} INVOKE [{target}] id={inv_id}")
                    if args:
                        print(f"    args: {json.dumps(args)[:500]}")
                elif msg_type == 3:  # Completion
                    if error:
                        print(f"{direction} ERROR id={inv_id}: {error}")
                    else:
                        print(f"{direction} RESULT id={inv_id}: {json.dumps(result)[:500] if result else 'null'}")
                elif msg_type == 6:  # Ping
                    pass  # Ignore pings
                else:
                    print(f"{direction} TYPE={msg_type}: {part[:200]}")
            except json.JSONDecodeError:
                if part.strip():
                    print(f"{direction} RAW: {part[:200]}")

addons = [SignalRLogger()]
