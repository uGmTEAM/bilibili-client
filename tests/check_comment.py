import json
import os
import sys
import urllib.parse
import urllib.request

UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36"
bvid = sys.argv[1]


def load_cookies():
    path = os.path.expanduser("~/.config/bilibili_client/cookies.json")
    with open(path, "r", encoding="utf-8") as handle:
        data = json.load(handle)
    entries = data.get("cookie_info", {}).get("cookies") or data.get("cookies") or []
    return {item["name"]: item["value"] for item in entries if "name" in item}


cookies = load_cookies()


def get(url, with_cookie):
    headers = {"User-Agent": UA, "Referer": f"https://www.bilibili.com/video/{bvid}"}
    if with_cookie:
        headers["Cookie"] = "; ".join(f"{key}={value}" for key, value in cookies.items())
    req = urllib.request.Request(url, headers=headers)
    return json.loads(urllib.request.urlopen(req, timeout=25).read().decode())


view = get(f"https://api.bilibili.com/x/web-interface/view?bvid={bvid}", False)
aid = view["data"]["aid"]
print("video stat.reply =", view["data"]["stat"]["reply"])

for with_cookie in (False, True):
    url = "https://api.bilibili.com/x/v2/reply?" + urllib.parse.urlencode(
        {"type": 1, "oid": aid, "pn": 1, "ps": 20, "sort": 0}
    )
    data = get(url, with_cookie)
    replies = (data.get("data") or {}).get("replies") or []
    print(f"cookie={with_cookie} code={data.get('code')} count={(data.get('data') or {}).get('page', {}).get('count')} items={len(replies)}")
    for reply in replies[:5]:
        print("  ", reply["rpid"], reply["member"]["uname"], reply["content"]["message"][:40])

# 尝试直接取指定 rpid 的详情
for rpid in sys.argv[2:]:
    url = f"https://api.bilibili.com/x/v2/reply/detail?type=1&oid={aid}&rpid={rpid}"
    data = get(url, True)
    print("detail", rpid, "->", data.get("code"), data.get("message"), json.dumps(data.get("data"), ensure_ascii=False)[:200])
