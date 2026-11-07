import json
import os
import sys
import urllib.request

mid = sys.argv[1]
UA = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36"


def get(url, cookie=None, referer=None):
    headers = {"User-Agent": UA}
    if cookie:
        headers["Cookie"] = cookie
    if referer:
        headers["Referer"] = referer
    req = urllib.request.Request(url, headers=headers)
    return json.loads(urllib.request.urlopen(req, timeout=25).read().decode())


def load_cookies():
    path = os.path.expanduser("~/.config/bilibili_client/cookies.json")
    try:
        with open(path, "r", encoding="utf-8") as handle:
            data = json.load(handle)
    except OSError:
        return {}
    entries = data.get("cookie_info", {}).get("cookies") or data.get("cookies") or []
    return {item["name"]: item["value"] for item in entries if "name" in item}


cookies = load_cookies()
spi = get("https://api.bilibili.com/x/frontend/finger/spi")
if spi.get("code") == 0:
    cookies["buvid3"] = spi["data"]["b_3"]
    cookies["buvid4"] = spi["data"]["b_4"]

cookie_header = "; ".join(f"{key}={value}" for key, value in cookies.items())
data = get(
    f"https://api.bilibili.com/x/space/arc/search?mid={mid}&ps=10&pn=1&order=pubdate",
    cookie=cookie_header,
    referer=f"https://space.bilibili.com/{mid}",
)
print("code:", data.get("code"), data.get("message"))
for item in (data.get("data") or {}).get("list", {}).get("vlist", []) or []:
    print(item.get("bvid"), item.get("title"), item.get("length"), item.get("created"))
