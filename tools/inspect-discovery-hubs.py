#!/usr/bin/env python3
"""Run on the phone: report field types only for the music discovery response."""
import collections
import json
from pathlib import Path
import urllib.parse
import urllib.request


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def main():
    try:
        state = json.loads((Path.home() / ".local/share/org.plexfreq/harbour-plexfreq/session.json").read_text())
        base = state["serverUrl"].rstrip("/")
        headers = {"Accept": "application/json", "X-Plex-Token": state["serverToken"], "X-Plex-Client-Identifier": state["clientIdentifier"]}
        opener = urllib.request.build_opener(NoRedirect)
        def get(path, params=None):
            uri = base + path + ("?" + urllib.parse.urlencode(params) if params else "")
            with opener.open(urllib.request.Request(uri, headers=headers), timeout=15) as response:
                return json.load(response)["MediaContainer"]
        sections = get("/library/sections").get("Directory", [])
        section = next(s for s in sections if s.get("type") == "artist")
        key = str(section["key"])
        if not key.isdigit():
            raise ValueError("invalid section")
        container = get("/hubs/sections/" + key, {"count": 12, "includeStations": 1, "includeMyMixes": 1})
        fields = collections.Counter()
        children = collections.Counter()
        hubs = container.get("Hub", [])
        for hub in hubs:
            for field, value in hub.items():
                fields[("hub", field, type(value).__name__)] += 1
            for child in ("Metadata", "Directory", "Playlist", "Station"):
                for item in hub.get(child) or []:
                    if not isinstance(item, dict):
                        children[(child, type(item).__name__)] += 1
                        continue
                    for field, value in item.items():
                        fields[("item", field, type(value).__name__)] += 1
        print(json.dumps({"hubs": len(hubs), "fields": [{"scope": s, "field": f, "type": t, "count": n} for (s, f, t), n in sorted(fields.items())], "otherChildren": len(children)}))
        return 0
    except Exception as error:
        print("Discovery inspection failed:", type(error).__name__)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
