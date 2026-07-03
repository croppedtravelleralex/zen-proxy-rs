#!/usr/bin/env python3
import json, subprocess, pathlib
text = chr(120)*50000
env = pathlib.Path("/etc/zen-proxy-rs/common.env").read_text()
key = [l.split("=",1)[1].strip() for l in env.splitlines() if l.startswith("PROXY_API_KEY=")][0]
for i in range(1,4):
    body = {"model":"deepseek-v4-flash","messages":[{"role":"user","content":text + chr(10) + "Reply PONG round " + str(i)}],"stream":False,"max_tokens":32}
    r = subprocess.run(["curl","-sf","-X","POST","http://127.0.0.1:4000/v1/chat/completions","-H","Authorization: Bearer " + key,"-H","Content-Type: application/json","-H","x-opencode-client: claude-code","-d",json.dumps(body)])
    if r.returncode: print("fail", i)
subprocess.run(["tail","-5","/var/log/zen-proxy-rs/audit/requests-2026-07-03.jsonl"])
