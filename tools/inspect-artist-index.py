#!/usr/bin/env python3
"""Read-only alphabet structure probe; print counts, not artists or credentials."""
import json
import pathlib
import urllib.parse
import urllib.request

try:
    state=json.loads((pathlib.Path.home()/".local/share/org.plexfreq/harbour-plexfreq/session.json").read_text())
    headers={"Accept":"application/json","X-Plex-Token":state["serverToken"],"X-Plex-Client-Identifier":state["clientIdentifier"]}
    def get(path,params=None):
        url=state["serverUrl"].rstrip("/")+path+("?"+urllib.parse.urlencode(params) if params else "")
        with urllib.request.urlopen(urllib.request.Request(url,headers=headers),timeout=15) as response:
            return json.load(response)["MediaContainer"]
    section=next(s for s in get("/library/sections")["Directory"] if s.get("type")=="artist")
    index=get("/library/sections/"+str(section["key"])+"/firstCharacter",{"type":8,"sort":"titleSort:asc"})
    groups=index.get("Directory") or []
    print(json.dumps({"rootFields":sorted(index.keys()),"groupCount":len(groups),"groupFields":sorted(groups[0].keys()) if groups else [],"indexedArtists":sum(int(g.get("size") or 0) for g in groups)}))
except Exception as error:
    print("Index probe failed:",type(error).__name__)
    raise SystemExit(1)
