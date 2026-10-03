#!/usr/bin/env python3
"""Read-only playlist occurrence schema probe: names/tokens/IDs never printed."""
import json
import pathlib
import urllib.parse
import urllib.request

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None

try:
    state = json.loads((pathlib.Path.home() / ".local/share/org.plexfreq/harbour-plexfreq/session.json").read_text())
    headers = {"Accept": "application/json", "X-Plex-Token": state["serverToken"], "X-Plex-Client-Identifier": state["clientIdentifier"]}
    opener = urllib.request.build_opener(NoRedirect)
    def get(path, params=None):
        url = state["serverUrl"].rstrip("/") + path + ("?" + urllib.parse.urlencode(params) if params else "")
        with opener.open(urllib.request.Request(url, headers=headers), timeout=15) as response:
            data = response.read(1024 * 1024 + 1)
            if len(data) > 1024 * 1024:
                raise ValueError("Response limit")
            return json.loads(data)["MediaContainer"]
    playlists = get("/playlists", {"playlistType": "audio"}).get("Metadata", [])
    print(json.dumps({"playlists": len(playlists), "smart": sum(bool(item.get("smart")) for item in playlists), "regularNonempty": sum(not item.get("smart") and item.get("leafCount", 0) > 0 for item in playlists)}))
    for item in playlists:
        if not item.get("leafCount"):
            continue
        key = item["ratingKey"]
        if not key.isdigit():
            raise ValueError("Non-numeric key")
        container = get("/playlists/" + key + "/items", {"X-Plex-Container-Start": 0, "X-Plex-Container-Size": 100})
        entries = container.get("Metadata", [])
        print(json.dumps({"smart": bool(item.get("smart")), "rootFieldTypes": {name: type(value).__name__ for name, value in container.items()}, "rows": len(entries), "firstEntryFieldTypes": {name: type(value).__name__ for name, value in entries[0].items()} if entries else {}}))
except Exception as error:
    print("Playlist entry probe failed:", type(error).__name__)
    raise SystemExit(1)
