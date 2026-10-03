#!/usr/bin/env python3
"""Read-only radio capability probe on the phone. Print counts/types, no values."""
import json
import pathlib
import urllib.error
import urllib.parse
import urllib.request
import collections


def main():
    state_path = pathlib.Path.home() / ".local/share/org.plexfreq/harbour-plexfreq/session.json"
    try:
        state = json.loads(state_path.read_text())
        base = state["serverUrl"].rstrip("/")
        headers = {"Accept": "application/json", "X-Plex-Product": "PlexFreq",
                   "X-Plex-Client-Identifier": state["clientIdentifier"],
                   "X-Plex-Token": state["serverToken"]}

        def get(path, params=None):
            request = urllib.request.Request(base + path + ("?" + urllib.parse.urlencode(params) if params else ""), headers=headers)
            with urllib.request.urlopen(request, timeout=15) as response:
                return json.load(response)["MediaContainer"]

        sections = get("/library/sections").get("Directory", [])
        section = next((s for s in sections if s.get("type") == "artist"), None)
        if not section:
            print("No music section")
            return 1
        root = get("/")
        result = {"serverPlexPass": bool(root["myPlexSubscription"]) if "myPlexSubscription" in root else None}
        if state.get("accountToken"):
            account_headers = dict(headers)
            account_headers["X-Plex-Token"] = state["accountToken"]
            try:
                with urllib.request.urlopen(urllib.request.Request("https://plex.tv/api/v2/user", headers=account_headers), timeout=15) as response:
                    account = json.load(response)
                subscription = account.get("subscription") or {}
                result["accountSubscriptionActive"] = bool(subscription.get("active"))
                result["accountLifetime"] = subscription.get("plan") == "lifetime"
            except urllib.error.HTTPError as error:
                result["accountHttpStatus"] = error.code
        prefs = get("/library/sections/" + str(section["key"]) + "/prefs").get("Setting", [])
        result["sonicSettings"] = {p["id"]:p.get("value") for p in prefs
                                  if p.get("id") in ("musicAnalysis", "enableSonicAnalysis", "sonicAnalysis")}
        for kind, number in [("artist", 8), ("album", 9), ("track", 10)]:
            page = get("/library/sections/" + str(section["key"]) + "/all",
                       {"type": number, "sort":"titleSort:asc", "X-Plex-Container-Start": 0, "X-Plex-Container-Size": 100})
            items = page.get("Metadata", [])
            if not items:
                result[kind] = {"sampleAvailable": False}
                continue
            key = str(items[0]["ratingKey"])
            if not key.isdigit():
                raise ValueError("Non-numeric identifier")
            metadata = get("/library/metadata/" + key, {"includeStations": 1})["Metadata"][0]
            stations = metadata.get("Stations")
            result[kind] = {"sampleAvailable": True,
                            "analysisFieldPresent": "musicAnalysisVersion" in metadata,
                            "sonicAnalyzed": metadata.get("musicAnalysisVersion") == 1,
                            "sampledCount":len(items),
                            "sampledAnalyzedCount":sum(i.get("musicAnalysisVersion") == 1 for i in items),
                            "descriptionPresent":bool(metadata.get("summary")),
                            "portraitPresent":bool(metadata.get("thumb")),
                            "backgroundPresent":bool(metadata.get("art")),
                            "imageCount":len(metadata.get("Image") or []),
                            "stationsShape": type(stations).__name__,
                            "stationsChildren": sorted(stations.keys()) if isinstance(stations, dict) else [],
                            "stationCount": len(stations) if isinstance(stations, list) else None}
            field_types = collections.Counter()
            for item in items:
                for field in ("summary", "art", "year", "index", "parentIndex", "Image"):
                    tag = "missing" if field not in item else type(item[field]).__name__
                    field_types[field + "." + tag] += 1
                if isinstance(item.get("Image"), list):
                    for image in item["Image"]:
                        field_types["Image.url." + type(image.get("url")).__name__] += 1
            result[kind]["fieldTypes"] = dict(field_types)
            result[kind]["negativeNumericFields"] = {f:sum(isinstance(i.get(f), int) and i[f] < 0 for i in items)
                                                     for f in ("year", "index", "parentIndex")}
            if kind != "artist":
                try:
                    nearest = get("/library/metadata/" + key + "/nearest", {"limit": 50})
                    neighbors = nearest.get("Metadata", [])
                    result[kind]["nearestCount"] = len(neighbors)
                    result[kind]["nearestTypes"] = sorted(set(i.get("type", "missing") for i in neighbors))
                except urllib.error.HTTPError as error:
                    result[kind]["nearestHttpStatus"] = error.code
        print(json.dumps(result, sort_keys=True))
        return 0
    except urllib.error.HTTPError as error:
        print("Capability HTTP status:", error.code)
    except Exception as error:
        print("Capability probe failed:", type(error).__name__)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
