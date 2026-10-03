#!/usr/bin/env python3
"""Read-only current-lyric/playlist schema probe. Output keys/types/counts only."""
import json
import pathlib
import urllib.parse
import urllib.request

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None

try:
    state = json.loads((pathlib.Path.home() / ".local/share/org.plexfreq/harbour-plexfreq/session.json").read_text())
    base = state["serverUrl"]
    headers = {"Accept": "application/json", "X-Plex-Token": state["serverToken"], "X-Plex-Client-Identifier": state["clientIdentifier"]}
    opener = urllib.request.build_opener(NoRedirect)
    def fetch(path):
        if not path.startswith("/") or path.startswith("//") or "\\" in path:
            raise ValueError("Nonrelative path")
        url = urllib.parse.urljoin(base, path)
        if urllib.parse.urlsplit(url).netloc != urllib.parse.urlsplit(base).netloc:
            raise ValueError("Origin mismatch")
        with opener.open(urllib.request.Request(url, headers=headers), timeout=15) as response:
            body = response.read(512 * 1024 + 1)
            if len(body) > 512 * 1024:
                raise ValueError("Response too large")
            return response.headers.get_content_type(), body
    def shape(value, depth=0):
        if depth > 10:
            return type(value).__name__
        if isinstance(value, dict):
            return {key: shape(item, depth + 1) for key, item in value.items()}
        if isinstance(value, list):
            return {"type": "list", "count": len(value), "firstShape": shape(value[0], depth + 1) if value else None}
        return type(value).__name__
    _, body = fetch("/playlists?playlistType=audio&X-Plex-Container-Size=100")
    playlists = json.loads(body)["MediaContainer"].get("Metadata", [])
    print(json.dumps({"playlistCount": len(playlists), "playlistFieldTypes": shape(playlists[0]) if playlists else {}, "countsPresent": sum(isinstance(item.get("leafCount"), int) for item in playlists), "durationsPresent": sum(isinstance(item.get("duration"), int) for item in playlists), "missingDurationEmptyCount": sum(item.get("leafCount") == 0 for item in playlists if not isinstance(item.get("duration"), int))}))
    playback = state.get("playback", {})
    queue = playback.get("queue", {})
    current = queue.get("current")
    items = queue.get("items", [])
    if isinstance(current, int) and 0 <= current < len(items):
        key = items[current]["ratingKey"]
        if not key.isdigit():
            raise ValueError("Non-numeric key")
        _, body = fetch("/library/metadata/" + key)
        metadata = json.loads(body)["MediaContainer"].get("Metadata", [])
        streams = [stream for item in metadata for media in item.get("Media", []) for part in media.get("Part", []) for stream in part.get("Stream", []) if stream.get("streamType") == 4 and stream.get("key")]
        print(json.dumps({"lyricStreamCount": len(streams)}))
        if streams:
            content_type, body = fetch(streams[0]["key"])
            try:
                payload = json.loads(body)
                print(json.dumps({"lyricContentType": content_type, "lyricBodyShape": shape(payload)}))
                docs = payload.get("MediaContainer", {}).get("Lyrics", []) if isinstance(payload, dict) else []
                lines = docs[0].get("Line", []) if docs else []
                print(json.dumps({"distinctLineShapes": [json.loads(item) for item in sorted({json.dumps(shape(line), sort_keys=True) for line in lines})]}))
            except json.JSONDecodeError:
                print(json.dumps({"lyricContentType": content_type, "lyricBodyFormat": "non-json", "lineCount": len(body.splitlines())}))
    else:
        print("No saved current track for lyric inspection")
except Exception as error:
    print("Lyrics/playlist schema probe failed:", type(error).__name__)
    raise SystemExit(1)
