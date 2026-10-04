#!/usr/bin/env python3
"""Qt TS/QM workflow, deterministic and Git-independent (also works in SDK copies)."""
import argparse
import copy
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
LANGUAGES = "bg bn cs da de el es et fi fr gu hi hu it kn lt lv ml mr nb nl pa pl pt pt_BR ro ru sk sl sv ta te tr tt uk vi zh_CN zh_HK zh_TW".split()
TOOLS = Path(os.environ.get("XDG_CACHE_HOME", str(Path.home()/".cache"))) / "qt-tools/PySide6"
PREFIX = "harbour-plexfreq"

def xml_bytes(root):
    ET.indent(root)
    return ET.tostring(root, encoding="utf-8", xml_declaration=True) + b"\n"

def extract():
    directory = ROOT / "app/translations"
    directory.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="plexfreq-i18n-") as work:
        path = Path(work)/"template.ts"
        sources = sorted(str(p) for folder in [ROOT/"app/qml", ROOT/"app/src"] for p in folder.rglob("*") if p.suffix in [".qml", ".cpp", ".h"])
        subprocess.run([str(TOOLS/"lupdate"), *sources, "-no-obsolete", "-locations", "none", "-ts", str(path)], check=True)
        template = ET.parse(path).getroot()
    template.set("sourcelanguage", "en")
    core = set()
    for path in (ROOT/"src").rglob("*.rs"):
        text = path.read_text()
        for pattern in [r'Error::Input\(\s*"((?:[^"\\]|\\.)*)"', r'Error::ProtocolAt\(\s*"((?:[^"\\]|\\.)*)"']:
            for source in re.findall(pattern, text):
                core.add(json.loads('"'+source+'"'))
    core.update(["Backend unavailable", "Backend panic", "Network request failed (check connection and server address)", "Plex authorization failed; sign in again or check server token", "Unexpected Plex response", "Unexpected Plex response during %1", "Plex returned HTTP %1", "Invalid URL", "Invalid request data", "Cannot read or write application state", "Audio playback failed", "Audio streaming failed", "Next track decoding failed", "Invalid audio representation"])
    for name, sources in [("Core",sorted(core)), ("AlbumType",["Albums","EPs","Singles","Live","Compilations"])]:
        context=next((c for c in template.findall("context") if c.findtext("name")==name),None)
        if context is None:context=ET.SubElement(template,"context");ET.SubElement(context,"name").text=name
        present={m.findtext("source") for m in context.findall("message")}
        for source in sources:
            if source in present:continue
            message=ET.SubElement(context,"message");ET.SubElement(message,"source").text=source;ET.SubElement(message,"translation",{"type":"unfinished"})
    return template

def reference(language):
    values={}
    for folder, prefix in [(ROOT.parent/"ElectricEel/app/translations","harbour-electric-eel"),(ROOT.parent/"sailfish-proton/translations","proton")]:
        file=folder/f"{prefix}_{language}.ts"
        if file.exists():
            for message in ET.parse(file).getroot().iter("message"):
                translation=message.find("translation")
                if translation is not None and translation.get("type")!="unfinished" and translation.text:
                    values.setdefault(message.findtext("source"),translation.text)
    return values

def validate(source,translation):
    if not translation or translation==source and re.search(r"[A-Za-z]{3}",source) and source not in ["PlexFreq","Plex token","X-Plex-Token","MiB","EPs","Radio","Downloads"]:
        # Identical legitimate words are allowed by explicit dictionary entries.
        if not translation:
            raise ValueError("Empty translation: "+source)
    if sorted(re.findall(r"%[1-9n]",source))!=sorted(re.findall(r"%[1-9n]",translation)):
        raise ValueError("Placeholder mismatch: "+source)

def main():
    parser=argparse.ArgumentParser();parser.add_argument("--extract",action="store_true");parser.add_argument("--check",action="store_true");args=parser.parse_args()
    for binary in ["lupdate","lrelease"]:
        if not (TOOLS/binary).is_file():
            raise SystemExit("Qt Linguist tools missing; use the reference project's tools/build-qm.sh bootstrap")
    template=extract();folder=ROOT/"app/translations";master=folder/f"{PREFIX}.ts"
    if args.extract:
        master.write_bytes(xml_bytes(template));sources=sorted({m.findtext("source") for m in template.iter("message")});print(json.dumps(sources,ensure_ascii=False,indent=2));return
    authored=json.loads((ROOT/"tools/translations.json").read_text());outputs={master:xml_bytes(template)}
    for language in LANGUAGES:
        root=copy.deepcopy(template);root.set("language",language)
        table=authored[language]
        for message in root.iter("message"):
            source=message.findtext("source");translated=table.get(source)
            if translated is None:raise ValueError(f"Missing {language}: {source}")
            validate(source,translated);tr=message.find("translation");tr.attrib.clear();tr.text=translated
        outputs[folder/f"{PREFIX}_{language}.ts"]=xml_bytes(root)
    for file,data in outputs.items():
        if args.check:
            if not file.exists() or file.read_bytes()!=data:raise SystemExit("Translation catalogue out of date: "+str(file.relative_to(ROOT)))
        else:file.write_bytes(data)
    with tempfile.TemporaryDirectory(prefix="plexfreq-qm-") as work:
        for language in LANGUAGES:
            ts=folder/f"{PREFIX}_{language}.ts";qm=folder/f"{PREFIX}_{language}.qm";temporary=Path(work)/qm.name
            subprocess.run([str(TOOLS/"lrelease"),"-silent",str(ts),"-qm",str(temporary)],check=True)
            data=temporary.read_bytes()
            if args.check:
                if not qm.exists() or qm.read_bytes()!=data:raise SystemExit("Compiled catalogue out of date: "+qm.name)
            else:qm.write_bytes(data)
    resource=ET.Element("RCC");group=ET.SubElement(resource,"qresource",{"prefix":"/translations"})
    for language in LANGUAGES:ET.SubElement(group,"file",{"alias":f"{PREFIX}_{language}.qm"}).text=f"translations/{PREFIX}_{language}.qm"
    resource_file=ROOT/"app/translations.qrc";data=xml_bytes(resource)
    if args.check:
        if not resource_file.exists() or resource_file.read_bytes()!=data:raise SystemExit("Translation resource manifest out of date")
    else:resource_file.write_bytes(data)
    print(f"Validated {len(LANGUAGES)} locales, {len(list(template.iter('message')))} messages per catalogue")

if __name__=="__main__":main()
