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
from qt_tools import ensure_tools

ROOT = Path(__file__).resolve().parents[1]
LANGUAGES = "bg bn cs da de el es et fi fr gu hi hu it kn lt lv ml mr nb nl pa pl pt pt_BR ro ru sk sl sv ta te tr tt uk vi zh_CN zh_HK zh_TW".split()
TOOLS = Path(os.environ.get("XDG_CACHE_HOME", str(Path.home()/".cache"))) / "qt-tools/PySide6"
PREFIX = "harbour-plexfreq"

def xml_bytes(root):
    ET.indent(root)
    return ET.tostring(root, encoding="utf-8", xml_declaration=True) + b"\n"

LITERAL = r'"((?:[^"\\]|\\.)*)"'
RAW_STRING = re.compile(r'b?r(#*)"')
CHAR_LITERAL = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'])'")

def rust_code(text):
    """Rust source with comments removed, char literals blanked and raw strings rewritten
    as plain string literals, so the scanners below only see code and "…" literals."""
    out, i = [], 0
    while i < len(text):
        c = text[i]
        if text.startswith("//", i):
            i = text.find("\n", i)
            i = len(text) if i < 0 else i
        elif text.startswith("/*", i):
            depth, i = 1, i+2
            while depth:
                if i >= len(text): raise ValueError("Unterminated block comment")
                if text.startswith("/*", i): depth, i = depth+1, i+2
                elif text.startswith("*/", i): depth, i = depth-1, i+2
                else: i += 1
        elif c in "br" and (raw := RAW_STRING.match(text, i)) and (i == 0 or not (text[i-1].isalnum() or text[i-1] == "_")):
            end = text.find('"'+raw.group(1), raw.end())
            if end < 0: raise ValueError("Unterminated raw string")
            out.append('"'+text[raw.end():end].replace("\\", "\\\\").replace('"', '\\"')+'"'); i = end+1+len(raw.group(1))
        elif c == '"':
            j = i+1
            while j < len(text) and text[j] != '"': j += 2 if text[j] == "\\" else 1
            if j >= len(text): raise ValueError("Unterminated string")
            out.append(text[i:j+1]); i = j+1
        elif c == "'" and (char := CHAR_LITERAL.match(text, i)):
            out.append("' '"); i = char.end()
        else:
            out.append(c); i += 1
    return "".join(out)

def rust_string(body):
    """Value of a Rust string literal body (between the quotes)."""
    def escape(match):
        e = match.group(1)
        if e[0] == "\n": return ""  # line continuation; leading whitespace is part of the match
        if e[0] == "x": return chr(int(e[1:], 16))
        if e[0] == "u": return chr(int(e[2:-1], 16))
        return {"n": "\n", "t": "\t", "r": "\r", "0": "\0", "\\": "\\", '"': '"', "'": "'"}[e]
    try:
        return re.sub(r"\\(\n\s*|x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)", escape, body, flags=re.S)
    except KeyError as error:
        raise ValueError(f"Unknown escape in Rust string: {body!r}") from error

def balanced(code, i, close):
    """Index after the bracket that closes the one before `i`; strings are skipped."""
    depth = 1
    while depth:
        if i >= len(code): raise ValueError("Unbalanced brackets")
        c = code[i]
        if c == '"':
            i += 1
            while code[i] != '"': i += 2 if code[i] == "\\" else 1
        elif c in "([{": depth += 1
        elif c in ")]}": depth -= 1
        i += 1
    return i

def without_tests(code):
    """Drop `#[cfg(test)] mod … { … }` blocks."""
    while match := re.search(r"#\[cfg\((?:test|all\(test\b[^\]]*\))\)\]\s*(?:#\[[^\]]*\]\s*)*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{", code):
        code = code[:match.start()]+code[balanced(code, match.end(), "}"):]
    return code

# Functions that turn their last argument into Error::ProtocolAt, shown as
# "Unexpected Plex response (%1)" with %1 translated in the Core context.
CONTEXT_FUNCTIONS = ("self.request", "bounded_json", "plex_json")

def protocol_contexts(code):
    """String literals passed as the last argument of CONTEXT_FUNCTIONS calls."""
    # Any receiver of `.request`; free functions also by path; optional turbofish.
    names = [r"\.request" if name == "self.request" else r"(?<![\w.])"+re.escape(name) for name in CONTEXT_FUNCTIONS]
    pattern = r"(?:"+"|".join(names)+r")(?:::<[^()]*>)?\("
    for match in re.finditer(pattern, code):
        end = balanced(code, match.end(), ")")
        args, depth, start, i = [], 0, match.end(), match.end()
        while i < end-1:
            c = code[i]
            if c == '"':
                i += 1
                while code[i] != '"': i += 2 if code[i] == "\\" else 1
            elif c in "([{": depth += 1
            elif c in ")]}": depth -= 1
            elif c == "," and depth == 0: args.append(code[start:i]); start = i+1
            i += 1
        args = [a.strip() for a in [*args, code[start:end-1]] if a.strip()]
        literal = re.fullmatch(LITERAL, args[-1], re.S) if args else None
        if literal: yield rust_string(literal.group(1))

# Error::Input constants that never reach the presentation (a navigation-cancelled
# reply is discarded by the runtime).
INTERNAL_CONSTANTS = {"CANCELLED_MESSAGE"}

def call_arguments(code, pattern):
    """The argument text of each call matching `pattern` (which ends at its "(")."""
    for match in re.finditer(pattern, code):
        yield match, code[match.end():balanced(code, match.end(), ")")-1]

def core_messages(sources):
    """User-visible messages in Rust sources: {path: text}, test modules skipped."""
    core, codes = set(), {path: without_tests(rust_code(text)) for path, text in sources.items()}
    constants = {name: rust_string(value) for code in codes.values()
                 for name, value in re.findall(r"const\s+([A-Z][A-Z0-9_]*)\s*:\s*&(?:'static\s+)?str\s*=\s*"+LITERAL, code)}
    for path, code in codes.items():
        # A function that turns a non-literal into Error::ProtocolAt must be listed in
        # CONTEXT_FUNCTIONS, or its callers' contexts would never be extracted.
        for _, argument in call_arguments(code, r"(?<![\w:])(?:crate::)?Error::ProtocolAt\("):
            argument = argument.strip()
            if re.fullmatch(LITERAL, argument, re.S): continue
            if re.fullmatch(r"(?:\w+::)*[A-Z][A-Z0-9_]*", argument): continue  # resolved below
            if not re.fullmatch(r"[a-z_]\w*", argument):
                raise ValueError(f"{path}: Error::ProtocolAt({argument}) is neither a literal, a constant nor a context parameter")
        for site in re.finditer(r"Error::ProtocolAt\(\s*[a-z_]\w*\s*\)", code):
            names = re.findall(r"\bfn\s+(\w+)", code[:site.start()])
            if not names or not ({names[-1], "self."+names[-1]} & set(CONTEXT_FUNCTIONS)):
                raise ValueError(f"{path}: {names[-1] if names else '?'} passes a context to Error::ProtocolAt; add it to CONTEXT_FUNCTIONS")
        core.update(protocol_contexts(code))
        # Every literal of an Error::Input/ProtocolAt argument (also inside a match); a
        # constant argument is resolved; a lowercase identifier is a variable or a pattern.
        for _, argument in call_arguments(code, r"(?<![\w:])(?:crate::)?Error::(?:Input|ProtocolAt)\("):
            argument = argument.strip()
            if re.fullmatch(r"(?:\w+::)*[A-Z][A-Z0-9_]*", argument):
                argument = argument.split("::")[-1]
                if argument in INTERNAL_CONSTANTS: continue
                if argument not in constants: raise ValueError(f"{path}: unresolved Error::Input({argument})")
                core.add(constants[argument])
            else:
                core.update(rust_string(value) for value in re.findall(LITERAL, argument, re.S))
        # OUTPUT_STUCK reaches the presentation through the runtime's audio banner.
        if "OUTPUT_STUCK" in constants and re.search(r"\bconst OUTPUT_STUCK\b", code):
            core.add(constants["OUTPUT_STUCK"])
    return core

def extract():
    directory = ROOT / "app/translations"
    directory.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="plexfreq-i18n-") as work:
        path = Path(work)/"template.ts"
        sources = sorted(str(p) for folder in [ROOT/"app/qml", ROOT/"app/src"] for p in folder.rglob("*") if p.suffix in [".qml", ".cpp", ".h"])
        subprocess.run([str(TOOLS/"lupdate"), *sources, "-no-obsolete", "-locations", "none", "-ts", str(path)], check=True)
        template = ET.parse(path).getroot()
    template.set("sourcelanguage", "en")
    core = core_messages({path: path.read_text() for path in (ROOT/"src").rglob("*.rs") if not path.name.endswith("_tests.rs")})
    core.update(["Backend unavailable", "Backend panic", "Network request failed (check connection and server address)", "Plex authorization failed; sign in again or check server token", "Unexpected Plex response", "Unexpected Plex response (%1)", "Plex returned HTTP %1", "Invalid URL", "Invalid request data", "Cannot read or write application state", "Audio playback failed", "Audio streaming failed", "Next track decoding failed", "Invalid audio representation", "Cache requires an unencoded audio response", "Server returned non-audio content for this track"])
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

# Names and units kept as written in every locale.
KEPT = re.compile(r"%[1-9n]|\b(?:X-Plex-Token|PlexFreq|Plexamp|Plex|MPRIS|PCM|HTTP|URL|Rust|MiB|kbps|dB|Wi-Fi|EPs?|\d+)\b")
SCRIPTS = {
    **dict.fromkeys(["bg","ru","uk","tt"], "\u0400-\u04ff"), "el":"\u0370-\u03ff",
    **dict.fromkeys(["hi","mr"], "\u0900-\u097f"), "bn":"\u0980-\u09ff", "pa":"\u0a00-\u0a7f",
    "gu":"\u0a80-\u0aff", "ta":"\u0b80-\u0bff", "te":"\u0c00-\u0c7f", "kn":"\u0c80-\u0cff",
    "ml":"\u0d00-\u0d7f", **dict.fromkeys(["zh_CN","zh_HK","zh_TW"], "\u4e00-\u9fff"),
}

def validate(language,source,translation,same=()):
    """Reject empty entries, placeholder mismatches, keyword lists joined by " · ",
    text without the locale's script, and English left in place unless the locale
    lists it as intentional in translations.json "_same"."""
    if not translation:
        raise ValueError("Empty translation: "+source)
    if sorted(re.findall(r"%[1-9n]",source))!=sorted(re.findall(r"%[1-9n]",translation)):
        raise ValueError("Placeholder mismatch: "+source)
    # Spaced: a Greek ano teleia (the semicolon, also U+00B7 after NFC) is not a list.
    if " · " in translation and "·" not in source:
        raise ValueError(f"Keyword list in {language}: {source}")
    words=re.search(r"[A-Za-z]",KEPT.sub("",source))
    if words and language in SCRIPTS and not re.search(f"[{SCRIPTS[language]}]",translation):
        raise ValueError(f"No {language} script: {source}")
    if words and translation==source and source not in same:
        raise ValueError(f"Untranslated {language}: {source}")

def main():
    parser=argparse.ArgumentParser();parser.add_argument("--extract",action="store_true");parser.add_argument("--check",action="store_true");args=parser.parse_args()
    global TOOLS
    TOOLS=ensure_tools()
    template=extract();folder=ROOT/"app/translations";master=folder/f"{PREFIX}.ts"
    if args.extract:
        master.write_bytes(xml_bytes(template));sources=sorted({m.findtext("source") for m in template.iter("message")});print(json.dumps(sources,ensure_ascii=False,indent=2));return
    authored=json.loads((ROOT/"tools/translations.json").read_text());outputs={master:xml_bytes(template)}
    if set(authored.get("_same",{}))-set(LANGUAGES):raise ValueError("Unknown _same locales: "+str(set(authored["_same"])-set(LANGUAGES)))
    for language in LANGUAGES:
        root=copy.deepcopy(template);root.set("language",language)
        table=authored[language];same=set(authored.get("_same",{}).get(language,[]))
        sources={m.findtext("source") for m in root.iter("message")}
        stale=sorted(set(table)-sources)+sorted(same-sources)
        if stale:raise ValueError(f"Stale {language}: {stale}")
        for message in root.iter("message"):
            source=message.findtext("source");translated=table.get(source)
            if translated is None:raise ValueError(f"Missing {language}: {source}")
            if source in same and translated!=source:raise ValueError(f"{language} lists a translated entry in _same: {source}")
            validate(language,source,translated,same);tr=message.find("translation");tr.attrib.clear();tr.text=translated
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
