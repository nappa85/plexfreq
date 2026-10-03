#!/usr/bin/env python3
"""Read-only phone diagnosis: emit field counts/types, never account or URL values.

Run over SSH on the phone with its normal state file. Credentials stay in the
phone process and go only to plex.tv over verified HTTPS. No response is saved.
"""
import collections
import json
import pathlib
import urllib.error
import urllib.request


def main():
    path = pathlib.Path.home() / ".local/share/org.plexfreq/harbour-plexfreq/session.json"
    try:
        state = json.loads(path.read_text())
        token = state.get("accountToken")
        if not token:
            print("No saved account authorization")
            return 1
        request = urllib.request.Request(
            "https://plex.tv/api/v2/resources?includeHttps=1&includeRelay=1",
            headers={
                "Accept": "application/json",
                "X-Plex-Product": "PlexFreq",
                "X-Plex-Version": "0.1.0",
                "X-Plex-Platform": "Linux",
                "X-Plex-Client-Identifier": state["clientIdentifier"],
                "X-Plex-Token": token,
            },
        )
        with urllib.request.urlopen(request, timeout=15) as response:
            resources = json.load(response)
        if not isinstance(resources, list):
            print("Discovery response is not a list")
            return 1
        counts = collections.Counter()
        fields = collections.Counter()
        for resource in resources:
            if not isinstance(resource, dict):
                counts["nonObjectEntries"] += 1
                continue
            provides = resource.get("provides")
            is_server = isinstance(provides, str) and "server" in provides.split(",")
            group = "server" if is_server else "nonServer"
            counts[group] += 1
            for field in ["name", "clientIdentifier", "provides", "accessToken", "connections"]:
                kind = "missing" if field not in resource else type(resource[field]).__name__
                fields[group + "." + field + "." + kind] += 1
        print(json.dumps({"counts": counts, "fieldTypes": fields}, sort_keys=True))
        return 0
    except urllib.error.HTTPError as error:
        print("Discovery HTTP status:", error.code)
    except Exception as error:
        # Exception messages may include response data or URLs; print class only.
        print("Diagnosis failed:", type(error).__name__)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
