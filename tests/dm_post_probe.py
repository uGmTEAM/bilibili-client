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
cookie_header = "; ".join(f"{key}={value}" for key, value in cookies.items())
csrf = cookies.get("bili_jct", "")


def get(url):
    req = urllib.request.Request(
        url,
        headers={
            "User-Agent": UA,
            "Cookie": cookie_header,
            "Referer": f"https://www.bilibili.com/video/{bvid}",
        },
    )
    return json.loads(urllib.request.urlopen(req, timeout=25).read().decode())


def post(url, form):
    body = urllib.parse.urlencode(form).encode()
    req = urllib.request.Request(
        url,
        data=body,
        headers={
            "User-Agent": UA,
            "Cookie": cookie_header,
            "Referer": f"https://www.bilibili.com/video/{bvid}",
            "Content-Type": "application/x-www-form-urlencoded",
        },
    )
    return json.loads(urllib.request.urlopen(req, timeout=25).read().decode())


view = get(f"https://api.bilibili.com/x/web-interface/view?bvid={bvid}")
if view.get("code") != 0:
    print("view failed:", view.get("code"), view.get("message"))
    sys.exit(1)
aid = view["data"]["aid"]
cid = view["data"]["pages"][0]["cid"]
print("aid", aid, "cid", cid)

base = {
    "type": "1",
    "oid": str(cid),
    "msg": "来自 bilic 客户端的测试弹幕",
    "progress": "5000",
    "color": "16777215",
    "fontsize": "25",
    "pool": "0",
    "mode": "1",
    "csrf": csrf,
}

variants = {
    "v2-dm-post+plat": dict(base, plat="1"),
    "v2-dm-post+bvid": dict(base, plat="1", bvid=bvid),
    "v2-dm-post+aid": dict(base, plat="1", aid=str(aid)),
}

for name, form in variants.items():
    url = "https://api.bilibili.com/x/v2/dm/post"
    try:
        result = post(url, form)
        print(name, "->", result.get("code"), result.get("message"))
        if result.get("code") == 0:
            print("works:", name, json.dumps(result.get("data"), ensure_ascii=False)[:200])
            break
    except Exception as error:  # noqa: BLE001
        print(name, "error", error)
