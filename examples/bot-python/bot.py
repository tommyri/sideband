#!/usr/bin/env python3
"""Sideband bot over HTTPS: `!echo <text>` and `!roll NdM`. Standard library only.

SIDEBAND_HOST, SIDEBAND_PORT (443), SIDEBAND_BOT_TOKEN, and for a self-signed
server SIDEBAND_FINGERPRINT (hex SHA-256 of its certificate). See docs/bots.md.
"""
import base64, hashlib, http.client, json, os, secrets, ssl, sys

HOST, PORT = os.environ["SIDEBAND_HOST"], int(os.environ.get("SIDEBAND_PORT", "443"))
TOKEN, PIN = os.environ["SIDEBAND_BOT_TOKEN"], os.environ.get("SIDEBAND_FINGERPRINT", "").lower()
AUTH = {"Authorization": "Bot " + TOKEN}


def connect(timeout=None):
    context = ssl.create_default_context()
    if PIN:  # Pinned like the desktop app: the leaf must match, the CA is not asked.
        context.check_hostname, context.verify_mode = False, ssl.CERT_NONE
    conn = http.client.HTTPSConnection(HOST, PORT, context=context, timeout=timeout)
    conn.connect()
    if PIN and hashlib.sha256(conn.sock.getpeercert(binary_form=True)).hexdigest() != PIN:
        raise SystemExit("certificate does not match SIDEBAND_FINGERPRINT")
    return conn


def send(channel_id, content):
    conn = connect(10)
    path = "/api/v1/channels/%s/messages" % base64.urlsafe_b64encode(base64.b64decode(channel_id)).decode()
    conn.request("POST", path, json.dumps({"content": content}), {**AUTH, "Content-Type": "application/json"})
    response = conn.getresponse()
    if response.status >= 300:
        print("send failed:", response.status, response.read().decode(), file=sys.stderr)
    conn.close()


def reply(content):
    if content.startswith("!echo "):
        return content[6:].strip() or None
    if not content.startswith("!roll"):
        return None
    count, _, sides = (content[5:].strip() or "1d6").partition("d")
    if not (count or "1").isdigit() or not sides.isdigit():
        return None
    count, sides = int(count or 1), int(sides)
    if not (1 <= count <= 20 and 2 <= sides <= 1000):
        return "I roll 1 to 20 dice of 2 to 1000 sides."
    dice = [secrets.randbelow(sides) + 1 for _ in range(count)]
    return "%dd%d: %s = %d" % (count, sides, " + ".join(map(str, dice)), sum(dice))


def main():
    me = None
    while True:  # A stream ends at its lifetime or when it falls behind: reconnect.
        conn = connect()
        conn.request("GET", "/api/v1/events?prefix=!", headers=AUTH)
        response = conn.getresponse()
        if response.status != 200:
            raise SystemExit("event stream refused: %d %s" % (response.status, response.read().decode()))
        for line in response:
            if not line.startswith(b"data:"):
                continue  # keep-alive comments and blank separators
            event = json.loads(line[5:])
            if "welcome" in event:
                me = event["welcome"]["user"]["id"]
                print("signed in as", event["welcome"]["user"]["username"], flush=True)
            elif "sessionRevoked" in event:
                raise SystemExit("bot token revoked")
            elif "messageCreated" in event:
                message = event["messageCreated"]["message"]
                text = reply(message.get("content", ""))
                if text and message.get("author", {}).get("id") != me:
                    send(message["channelId"], text)
        conn.close()


if __name__ == "__main__":
    main()
