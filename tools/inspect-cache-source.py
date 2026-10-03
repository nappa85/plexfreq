#!/usr/bin/env python3
"""Compare one bounded candidate's Part size with HTTP framing; values stay private."""
import json
import pathlib
import urllib.parse
import urllib.request


def main():
    try:
        state=json.loads((pathlib.Path.home()/".local/share/org.plexfreq/harbour-plexfreq/session.json").read_text())
        base=state["serverUrl"].rstrip("/")
        headers={"Accept":"application/json","X-Plex-Token":state["serverToken"],"X-Plex-Client-Identifier":state["clientIdentifier"]}
        def get(path,params=None):
            url=base+path+("?"+urllib.parse.urlencode(params) if params else "")
            with urllib.request.urlopen(urllib.request.Request(url,headers=headers),timeout=15) as response:
                return json.load(response)["MediaContainer"]
        section=next(i for i in get("/library/sections")["Directory"] if i.get("type")=="artist")
        page=get("/library/sections/"+str(section["key"])+"/all",{"type":10,"sort":"titleSort:asc","X-Plex-Container-Start":0,"X-Plex-Container-Size":100})
        selected=None
        for item in page.get("Metadata",[]):
            parts=(item.get("Media") or [{}])[0].get("Part") or []
            if parts and 0<int(parts[0].get("size") or 0)<=8*1024*1024:
                selected=parts[0]; break
        if selected is None:
            print("No bounded candidate"); return 1
        url=urllib.parse.urljoin(base+"/",selected["key"])
        if urllib.parse.urlsplit(url).netloc!=urllib.parse.urlsplit(base).netloc:
            print("Cross-origin source rejected"); return 1
        media_headers=dict(headers); media_headers.update({"Accept":"*/*","Range":"bytes=0-0","Accept-Encoding":"identity"})
        with urllib.request.urlopen(urllib.request.Request(url,headers=media_headers),timeout=15) as response:
            print(json.dumps({"metadataBytes":selected.get("size"),"httpStatus":response.status,
                              "contentLength":response.headers.get("Content-Length"),"contentRange":response.headers.get("Content-Range"),
                              "contentType":response.headers.get("Content-Type"),"etagPresent":bool(response.headers.get("ETag"))}))
            response.read(1)  # never dump or download the full representation
        return 0
    except Exception as error:
        print("Source inspection failed:",type(error).__name__)
        return 1


if __name__=="__main__":
    raise SystemExit(main())
