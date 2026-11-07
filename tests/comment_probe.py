import json
import os
import sys
import time
import urllib.parse
import urllib.request

UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36"
bvid = sys.argv[1]
message = sys.argv[2]


def load_cookies():
    path = os.path.expanduser("~/.config/bilibili_client/cookies.json")
    with open(path, "r", encoding="utf-8") as handle:
        data = json.load(handle)
    entries = data.get("cookie_info", {}).get("cookies") or data.get("cookies") or []
    return {item["name"]: item["value"] for item in entries if "name" in item}


cookies = load_cookies()
cookie_header = "; ".join(f"{key}={value}" for key, value in cookies.items())
referer = f"https://www.bilibili.com/video/{bvid}"


def request(url, data=None, with_cookie=True):
    headers = {"User-Agent": UA, "Referer": referer}
    if with_cookie:
        headers["Cookie"] = cookie_header
    if data is not None:
        headers["Content-Type"] = "application/x-www-form-urlencoded"
        data = urllib.parse.urlencode(data).encode()
    req = urllib.request.Request(url, data=data, headers=headers)
    return json.loads(urllib.request.urlopen(req, timeout=25).read().decode())


view = request(f"https://api.bilibili.com/x/web-interface/view?bvid={bvid}", with_cookie=False)
aid = view["data"]["aid"]
print("aid", aid, "before stat.reply =", view["data"]["stat"]["reply"])

form = {
    "type": "1",
    "oid": str(aid),
    "message": message,
    "plat": "1",
    "csrf": cookies.get("bili_jct", ""),
}
added = request("https://api.bilibili.com/x/v2/reply/add", form)
print("add ->", json.dumps(added, ensure_ascii=False)[:400])

for step in range(3):
    time.sleep(8)
    view = request(f"https://api.bilibili.com/x/web-interface/view?bvid={bvid}", with_cookie=False)
    listing = request(
        "https://api.bilibili.com/x/v2/reply?"
        + urllib.parse.urlencode({"type": 1, "oid": aid, "pn": 1, "ps": 20, "sort": 0})
    )
    replies = (listing.get("data") or {}).get("replies") or []
    print(
        f"poll {step + 1}: stat.reply={view['data']['stat']['reply']} list_count="
        f"{(listing.get('data') or {}).get('page', {}).get('count')} items={len(replies)}"
    )
    for reply in replies[:5]:
        print("   ", reply["rpid"], reply["member"]["uname"], reply["content"]["message"][:40])
