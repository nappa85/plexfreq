#!/usr/bin/env python3
"""Metadata schema diagnostic; print types only, never values or credentials."""
import json
import pathlib
import urllib.request

try:
    state = json.loads((pathlib.Path.home() / ".local/share/org.plexfreq/harbour-plexfreq/session.json").read_text())
    headers = {"Accept": "application/json", "X-Plex-Token": state["serverToken"], "X-Plex-Client-Identifier": state["clientIdentifier"]}
    request = urllib.request.Request(state["serverUrl"].rstrip("/") + "/library/sections", headers=headers)
    with urllib.request.urlopen(request, timeout=15) as response:
        container = json.load(response)["MediaContainer"]
    print(json.dumps({"rootTypes": {key: type(value).__name__ for key, value in container.items()}, "sectionTypes": [{key: type(value).__name__ for key, value in item.items()} for item in container.get("Directory", [])]}))
except Exception as error:
    print("Library schema probe failed:", type(error).__name__)
    raise SystemExit(1)
