#!/usr/bin/env python3
"""Expand authored locale vocabulary into complete compact UI/message catalogues.

Shared reference translations are reused for exact source matches. New technical
messages deliberately use compact localized clauses, not untranslated English.
The expanded JSON is self-contained; building PlexFreq never needs sibling repos.
"""
import json
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path
from translations import ROOT, LANGUAGES, reference

SIMPLE = {
    "Albums":"albums", "Artists":"artists", "Tracks":"tracks", "Track":"track",
    "Playlist":"playlist", "Playlists":"playlists", "Queue":"queue", "Library":"library",
    "Radio":"radio", "Lyrics":"lyrics", "Year":"year", "Title":"title", "Sort":"sort",
    "Search":"search", "Browse":"library", "Mix":"mix", "Connection":"connection", "Connect":"connect",
    "Save":"save", "Back":"up", "Off":"off", "Downloaded":"downloaded",
    "Downloading":"downloading", "Downloads":"downloads", "Manage":"manager",
    "Crossfade":"crossfade", "Equalizer":"equalizer", "Compilations":"compilations",
    "Singles":"singles", "Live":"live", "Retry":"retry", "Shuffle":"shuffle", "Repeat":"repeat",
    "Read more":"readmore", "Read less":"readless", "Recently added":"recentadded",
    "Recently played":"recentplayed", "Normalize loudness":"normalization", "EPs":"=EP",
    "X-Plex-Token":"=X-Plex-Token",
}
PHRASES = {
    "Page actions":"settings", "More destinations":"library",
    "Use the pull-up menu for connection settings":"=↑ connection settings",
    "Automatically refresh download plans":"autoplay refresh downloads", "Refresh interval":"refresh duration", "%1 hours":"=%1 =h", "Refreshing download plans…":"refresh downloads loading",
    "Library filters":"library filter", "Smart playlist":"smart playlists", "Filtered music":"filter audio", "Create smart playlist":"create smart playlists", "Edit smart filters":"smart filter settings",
    "Any":"all", "Title contains":"title filter", "Year from":"year start", "Year to":"year limit", "Minimum rating":"favorites limit", "Maximum tracks":"limit tracks", "Unplayed only":"play none",
    "Most played":"history play", "Customize discovery":"search settings", "Show all in this group":"open all types", "Listening insights":"history settings", "Period":"duration", "All history":"all history", "%1 days":"=%1 =d",
    "%1 qualified plays · %2 listened":"%1 play %2 history", "%1 plays · %2 listened":"%1 play %2 history",
    "Based on locally measured qualified plays since insights tracking began.":"history local play metadata",
    "Add sonic waypoint":"add =Sonic source", "Clear sonic waypoints":"clear =Sonic source", "Sonic journey (%1 tracks)":"=Sonic mix %1 tracks",
    "These smart-playlist rules cannot be represented by this editor.":"smart playlists filter readonly",
    "%1 kbps":"=%1 =kbps", "%1 minutes":"=%1 =min", "%1 dB":"=%1 =dB",
    "Original audio":"source audio", "Streaming quality":"audio settings",
    "Plex codec fallback":"=Plex decoder retry", "Wi-Fi audio":"=Wi-Fi audio", "Mobile audio":"network audio", "Download audio":"downloads audio",
    "Headroom":"audio volume range", "Album gain":"albums normalization", "Track gain":"track normalization", "Automatic gain":"autoplay normalization", "Normalization mode":"normalization settings",
    "Discovery home":"search library", "Sonically similar":"audio similar",
    "Sonic Adventure":"=Sonic =Adventure", "Start sonic adventure here":"start =Sonic =Adventure", "Sonic Adventure to this track":"=Sonic =Adventure track",
    "Radio download length":"radio downloads duration", "Download radio · %1 minutes":"downloads radio =%1 =min", "Download %1 minutes":"downloads =%1 =min",
    "Planned audio: %1":"audio duration %1", "Refresh download plan":"refresh downloads", "Play downloaded tracks":"play downloaded tracks",
    "Quality changes apply to following tracks and downloads. Completed local audio is preferred.":"audio settings next tracks downloads ready audio",
    "Plex token":"=Plex token",
    "%1 tracks":"%1 tracks", "1 track":"=1 track", "%1 seconds":"%1 seconds",
    "%1 tracks downloaded · %2 MiB used":"%1 tracks downloaded %2 =MiB used",
    "%1/%2 tracks · %3 MiB":"=%1/%2 tracks %3 =MiB",
    "%1 MiB audio cache used":"%1 =MiB audio cache used", "%1 tracks ready offline":"%1 tracks ready offline",
    "Current track: %1 MiB received":"current track %1 =MiB received",
    "Pending listening history: %1":"pending history %1",
    "Play mix (%1 seeds)":"play mix %1 source",
    "Rust audio engine · gapless PCM output":"=Rust audio gapless =PCM output",
    "PlexFreq 0.1.0 · Direct-stream music player":"=PlexFreq =0.1.0 audio play",
    "Find your frequency.":"frequency search", "Favorites · 5 stars":"favorites =★ =5",
    "★ Favorite":"=★ favorites", "☆ Add to favorites":"=☆ add favorites",
    "No description provided by Plex.":"description unavailable =Plex",
    "No lyrics provided by Plex.":"lyrics unavailable =Plex", "No similar artists returned by Plex.":"similar artists unavailable =Plex",
    "Waiting for a confirmed Wi-Fi connection. Streaming remains available.":"downloads waiting =Wi-Fi audio play online",
    "Waiting for confirmed Wi-Fi; streaming remains available.":"downloads waiting =Wi-Fi audio play online",
    "The current track and next %1 queued tracks are cached. Download an album before travelling; only completed downloads work offline.":"cache current track next %1 tracks downloads albums ready offline",
    "Uses Plex gain metadata when available. Tracks within the same album keep gapless transitions without crossfade.":"=Plex metadata normalization albums gapless crossfade off",
    "Smart playlist contents are managed by Plex filters. New playlists use the selected tracks/album, or the queue.":"smart playlists =Plex filter new playlists tracks albums queue",
    "Smart playlists use Plex filters. New playlists use selected tracks/album, or the queue.":"smart playlists =Plex filter new playlists tracks albums queue",
    "Plex authorization failed; sign in again or check server token":"=Plex signin failed check server token",
    "Network request failed (check connection and server address)":"network request failed check connection server",
    "Plex returned HTTP %1":"=Plex =HTTP %1 failed",
    "Unexpected Plex response during %1":"=Plex data invalid %1",
    "Cache limit must be 64–8192 MiB and look-ahead 0–20 tracks":"cache limit =64–8192 =MiB next =0–20 tracks",
    "Server URL must be an HTTP(S) origin, e.g. http://192.168.1.10:32400":"server =URL =HTTP(S) =http://192.168.1.10:32400",
    "Or connect directly using a server URL and Plex token:":"connect server =URL =Plex token",
}
ALIASES = {
    "nothing":"none","queued":"queue","playing":"play","starting":"start","already":"current",
    "browse":"library","artwork":"photos","text":"lyrics","spans":"lyrics","span":"lyrics","lines":"lyrics","created":"create",
    "cached":"cache","caching":"cache","libraries":"library","streaming":"stream","stream":"audio",
    "engine":"audio","framework":"audio","pipeline":"audio","buffer":"audio","lyrics":"lyrics",
    "listening":"history","request":"request","response":"data","authorization":"signin",
    "application":"settings","backend":"audio","panic":"failed","unencoded":"invalid",
    "upcoming":"next","look-ahead":"next","grouping":"types","group":"types","types":"types",
    "occurrence":"track","details":"metadata","unavailable":"unavailable","unexpected":"invalid",
    "unknown":"invalid","relative":"relative","numeric":"identifier","expired":"expired",
    "corrupt":"corrupt","cancelled":"cancelled","inconsistent":"inconsistent","interrupted":"interrupted",
    "incomplete":"incomplete","initialization":"initialization","creation":"create","allocation":"allocation",
    "decoding":"decoder","decoded":"decoder","seeking":"volume","seek":"volume","gain":"normalization",
    "loudness":"normalization","unpin":"unpin","downloadable":"downloads","download":"downloads",
    "playback":"play","playable":"play","player":"play","repeat":"repeat","resume":"resume",
    "back":"up","more":"readmore","less":"readless","loading":"loading","description":"description",
    "exceeds":"limit","maximum":"limit","limited":"limit","limit":"limit","bytes":"size",
    "part":"data","length":"duration","range":"range","filters":"filter","controlled":"filter",
    "without":"off","none":"none","no":"none","not":"unavailable","cannot":"failed","only":"readonly",
    "read-only":"readonly","first":"start","start":"start","new":"new","seed":"mix","seeds":"mix",
    "invalid":"invalid","failed":"failed","failure":"failed","cancel":"cancelled","clear":"clear",
    "use":"used","used":"used","returned":"received","return":"received","received":"received",
    "active":"current","current":"current","saved":"saved","save":"save","open":"open","tap":"choose",
    "swipe":"move","updated":"refresh","refresh":"refresh","sync":"sync","identity":"identifier",
    "identifier":"identifier","alphabet":"artists","index":"index","letter":"index","sign":"signin",
    "login":"signin","sign-in":"signin","direct":"connection","directly":"connection","automatically":"autoplay",
    "related":"similar","sonic":"similar","recommendations":"similar","expected":"valid","valid":"valid",
    "cancel":"cancelled","removed":"remove","removal":"remove","include":"add","include":"add",
    "count":"count","chooser":"choose","state":"state","order":"order","worker":"worker",
    "relative":"relative","cross-origin":"crossorigin","discovery":"search","discover":"search",
}

ITALIAN = {
    "Automatically refresh download plans":"Aggiorna automaticamente i piani di download", "Refresh interval":"Intervallo di aggiornamento", "%1 hours":"%1 ore", "Refreshing download plans…":"Aggiornamento dei piani di download…",
    "Library filters":"Filtri della libreria", "Smart playlist":"Playlist intelligente", "Filtered music":"Musica filtrata", "Create smart playlist":"Crea una playlist intelligente", "Edit smart filters":"Modifica i filtri intelligenti", "Any":"Qualsiasi",
    "Title contains":"Il titolo contiene", "Year from":"Dall’anno", "Year to":"Fino all’anno", "Minimum rating":"Valutazione minima", "Maximum tracks":"Numero massimo di brani", "Unplayed only":"Solo brani mai ascoltati", "Most played":"Più ascoltati",
    "Customize discovery":"Personalizza Scopri", "Show all in this group":"Mostra tutto nel gruppo", "Listening insights":"Statistiche di ascolto", "Period":"Periodo", "All history":"Tutta la cronologia", "%1 days":"%1 giorni", "%1 qualified plays · %2 listened":"%1 ascolti qualificati · %2 di ascolto", "%1 plays · %2 listened":"%1 ascolti · %2 di ascolto",
    "Based on locally measured qualified plays since insights tracking began.":"Dati basati sugli ascolti qualificati misurati localmente dall’inizio del rilevamento delle statistiche.", "Add sonic waypoint":"Aggiungi una tappa sonica", "Clear sonic waypoints":"Svuota le tappe soniche", "Sonic journey (%1 tracks)":"Percorso sonico (%1 brani)", "These smart-playlist rules cannot be represented by this editor.":"Le regole di questa playlist intelligente non possono essere rappresentate da questo editor.",
    "Invalid music filter values":"Valori dei filtri musicali non validi", "Invalid discovery layout":"Configurazione di Scopri non valida", "Choose a refresh interval of 1, 6, 12 or 24 hours":"Scegli un intervallo di aggiornamento di 1, 6, 12 o 24 ore", "Choose 7, 30, 90 days or all listening history":"Scegli 7, 30, 90 giorni oppure tutta la cronologia", "Choose 2–8 tracks for a sonic journey":"Scegli 2–8 brani per un percorso sonico", "Sonic journey exceeds 250 tracks":"Il percorso sonico supera 250 brani",
    "Original audio":"Audio originale", "Streaming quality":"Qualità audio", "Wi-Fi audio":"Audio su Wi-Fi", "Mobile audio":"Audio su rete mobile", "Download audio":"Audio dei download", "Plex codec fallback":"Conversione Plex in caso di codec non supportato",
    "Quality changes apply to following tracks and downloads. Completed local audio is preferred.":"Le modifiche si applicano ai brani successivi e ai download. L’audio già scaricato ha la precedenza.",
    "%1 minutes":"%1 minuti", "Radio download length":"Durata dei download radio", "Download radio · %1 minutes":"Scarica radio · %1 minuti", "Download %1 minutes":"Scarica %1 minuti", "Planned audio: %1":"Audio nel piano: %1", "Refresh download plan":"Aggiorna il piano di download", "Play downloaded tracks":"Riproduci i brani scaricati", "Refresh":"Aggiorna", "Play":"Riproduci",
    "Page actions":"Azioni della pagina", "More destinations":"Altre destinazioni",
    "Discover":"Scopri", "Discovery home":"Scopri la tua musica", "Sonically similar":"Somiglianze sonore", "Sonic Adventure":"Percorso sonico", "Start sonic adventure here":"Inizia un percorso sonico da qui", "Sonic Adventure to this track":"Percorso sonico fino a questo brano",
    "Headroom":"Margine dinamico", "Normalization mode":"Modalità di normalizzazione", "Album gain":"Guadagno dell’album", "Track gain":"Guadagno del brano", "Automatic gain":"Guadagno automatico",
    "Choose original audio or 64–320 kbps":"Scegli l’audio originale oppure 64–320 kbps", "Choose 30–480 minutes for a radio download":"Scegli 30–480 minuti per un download radio", "Go online to refresh downloads":"Vai online per aggiornare i download", "Choose a music item":"Scegli un elemento musicale", "Choose a playlist, album or radio download":"Scegli una playlist, un album o una radio da scaricare", "Track duration is needed for a timed download":"Per un download a tempo è necessaria la durata dei brani", "Radio playback requires a connection":"La riproduzione della radio richiede una connessione",
    "Search artists, albums and tracks":"Cerca artisti, album e brani",
    "Add to playlist":"Aggiungi alla playlist", "Add to queue":"Aggiungi alla coda",
    "Play next":"Riproduci come successivo", "Up next":"In coda", "Now playing":"In riproduzione",
    "Nothing playing":"Nessun brano in riproduzione", "Nothing is queued":"La coda è vuota",
    "Downloaded music":"Musica scaricata", "Download manager":"Gestione download",
    "Download only on Wi-Fi":"Scarica solo tramite Wi-Fi", "Pause downloads":"Metti in pausa i download",
    "Resume downloads":"Riprendi i download", "Pin for offline listening":"Mantieni per l’ascolto offline",
    "Unpin download":"Non mantenere il download offline", "Remove download":"Rimuovi download",
    "Cancel plan / keep files":"Annulla il piano e conserva i file", "Cancel plan":"Annulla il piano",
    "Remove files":"Elimina i file", "Audio settings":"Impostazioni audio",
    "Normalize loudness":"Uniforma il volume", "Reset equalizer":"Ripristina l’equalizzatore",
    "Uses Plex gain metadata when available. Tracks within the same album keep gapless transitions without crossfade.":"Usa i metadati di guadagno di Plex quando disponibili. I brani dello stesso album vengono riprodotti senza interruzioni e senza dissolvenza incrociata.",
    "Rust audio engine · gapless PCM output":"Motore audio Rust · uscita PCM senza interruzioni",
    "Create playlist":"Crea playlist", "Create playlist from this":"Crea una playlist da questo elemento",
    "Create playlist from tracks/queue":"Crea una playlist dai brani o dalla coda",
    "Rename playlist":"Rinomina playlist", "Delete playlist":"Elimina playlist", "Playlist name":"Nome della playlist",
    "Move playlist entry up":"Sposta la voce della playlist su", "Move playlist entry down":"Sposta la voce della playlist giù",
    "Remove from playlist":"Rimuovi dalla playlist", "Remove from queue":"Rimuovi dalla coda",
    "Add as mix seed":"Aggiungi come punto di partenza del mix", "Play mix (%1 seeds)":"Riproduci mix (%1 punti di partenza)",
    "Clear mix seeds":"Svuota i punti di partenza del mix", "Autoplay related music when queue ends":"Continua con musica simile al termine della coda",
    "Autoplay related music at queue end":"Continua con musica simile al termine della coda",
    "Group albums by type":"Raggruppa gli album per tipo", "Group album types":"Raggruppa gli album per tipo",
    "Smart playlist contents are managed by Plex filters. New playlists use the selected tracks/album, or the queue.":"Il contenuto delle playlist intelligenti è gestito dai filtri di Plex. Le nuove playlist usano i brani o l’album selezionati oppure la coda.",
    "Smart playlists use Plex filters. New playlists use selected tracks/album, or the queue.":"Le playlist intelligenti usano i filtri di Plex. Le nuove playlist usano i brani o l’album selezionati oppure la coda.",
    "Waiting for a confirmed Wi-Fi connection. Streaming remains available.":"In attesa di una connessione Wi-Fi verificata. La riproduzione in streaming rimane disponibile.",
    "Waiting for confirmed Wi-Fi; streaming remains available.":"In attesa di una connessione Wi-Fi verificata; lo streaming rimane disponibile.",
    "The current track and next %1 queued tracks are cached. Download an album before travelling; only completed downloads work offline.":"Vengono memorizzati il brano attuale e i %1 successivi in coda. Scarica un album prima di viaggiare: solo i download completati funzionano offline.",
    "Complete sign-in in your browser, then return here.":"Completa l’accesso nel browser, poi torna qui.",
    "Use the pull-down menu for connection settings":"Usa il menu a tendina per le impostazioni di connessione",
    "Connect to your Plex music server to start listening.":"Connettiti al server musicale Plex per iniziare l’ascolto.",
    "Select a server to open its music libraries.":"Seleziona un server per aprire le sue librerie musicali.",
    "Tap a server to open its music libraries.":"Tocca un server per aprire le sue librerie musicali.",
    "Refresh the list to find your Plex servers.":"Aggiorna l’elenco per trovare i tuoi server Plex.",
    "Sign in with Plex in browser":"Accedi a Plex nel browser", "Sign out and clear saved tokens":"Esci ed elimina i token salvati",
    "Find your frequency.":"Trova la tua frequenza.", "Similar artists":"Artisti simili",
    "Similar artists are unavailable right now.":"Gli artisti simili non sono disponibili al momento.",
    "No lyrics provided by Plex.":"Plex non ha fornito il testo della canzone.",
    "Lyrics unavailable while offline or on this server.":"Testi non disponibili offline o su questo server.",
    "%1 tracks":"%1 brani", "1 track":"1 brano", "%1 seconds":"%1 secondi",
    "%1 tracks downloaded · %2 MiB used":"%1 brani scaricati · %2 MiB utilizzati",
    "%1 tracks ready offline":"%1 brani disponibili offline", "%1 MiB audio cache used":"%1 MiB di cache audio utilizzati",
    "%1/%2 tracks · %3 MiB":"%1/%2 brani · %3 MiB", "Pending listening history: %1":"Ascolti da sincronizzare: %1",
    "Current track: %1 MiB received":"Brano attuale: %1 MiB ricevuti",
    "Offline · showing saved library metadata":"Offline · metadati salvati della libreria",
    "Browse saved library offline":"Esplora la libreria salvata offline", "Offline library":"Libreria offline",
    "Sync listening history":"Sincronizza la cronologia degli ascolti", "Sync history":"Sincronizza la cronologia",
    "★ Favorite":"★ Preferito", "☆ Add to favorites":"☆ Aggiungi ai preferiti",
    "Audio playback failed":"Riproduzione audio non riuscita", "Audio streaming failed":"Streaming audio non riuscito",
    "Network request failed (check connection and server address)":"Richiesta di rete non riuscita: verifica la connessione e l’indirizzo del server",
    "Plex authorization failed; sign in again or check server token":"Autorizzazione Plex non riuscita: accedi di nuovo o verifica il token del server",
    "Unexpected Plex response during %1":"Risposta Plex inattesa durante %1", "Plex returned HTTP %1":"Plex ha restituito HTTP %1",
    "Cannot read or write application state":"Impossibile leggere o salvare lo stato dell’applicazione"
}

SHOW_ALL = {
    "bg":"Покажи всички", "bn":"সব দেখান", "cs":"Zobrazit vše", "da":"Vis alle",
    "de":"Alle anzeigen", "el":"Εμφάνιση όλων", "es":"Mostrar todo", "et":"Näita kõiki",
    "fi":"Näytä kaikki", "fr":"Tout afficher", "gu":"બધું બતાવો", "hi":"सभी दिखाएँ",
    "hu":"Összes megjelenítése", "it":"Mostra tutto", "kn":"ಎಲ್ಲವನ್ನೂ ತೋರಿಸಿ",
    "lt":"Rodyti viską", "lv":"Rādīt visu", "ml":"എല്ലാം കാണിക്കുക", "mr":"सर्व दाखवा",
    "nb":"Vis alle", "nl":"Alles tonen", "pa":"ਸਭ ਦਿਖਾਓ", "pl":"Pokaż wszystko",
    "pt":"Mostrar tudo", "pt_BR":"Mostrar tudo", "ro":"Afișează tot", "ru":"Показать всё",
    "sk":"Zobraziť všetko", "sl":"Prikaži vse", "sv":"Visa alla", "ta":"அனைத்தையும் காட்டு",
    "te":"అన్నీ చూపించు", "tr":"Tümünü göster", "tt":"Барысын күрсәт", "uk":"Показати все",
    "vi":"Hiển thị tất cả", "zh_CN":"显示全部", "zh_HK":"顯示全部", "zh_TW":"顯示全部"
}

def compact(source,words):
    if source in SIMPLE:return words.get(SIMPLE[source], SIMPLE[source][1:] if SIMPLE[source].startswith("=") else SIMPLE[source])
    if source in PHRASES:
        parts=PHRASES[source].split()
        return " · ".join(part[1:] if part.startswith("=") else words[part] if part in words else part for part in parts)
    tokens=re.findall(r"%[1-9n]|[0-9]+(?:[–/][0-9]+)?|[A-Za-z]+(?:-[A-Za-z]+)?|[★☆]",source)
    parts=[]
    for token in tokens:
        lower=token.lower()
        if token.startswith("%") or token[0].isdigit() or token in ["★","☆","Plex","PlexFreq","Rust","PCM","UTF","HTTP","URL","MiB","Wi-Fi","Content-Range"]:
            translated=token
        else:
            key=ALIASES.get(lower,lower)
            if key not in words:continue
            translated=words[key]
        if not parts or translated not in parts:parts.append(translated)
    if not parts:raise ValueError("No translation recipe: "+source)
    # Repeated placeholders are meaningful even when other clauses are deduplicated.
    for placeholder in re.findall(r"%[1-9n]",source):
        if parts.count(placeholder)<re.findall(r"%[1-9n]",source).count(placeholder):parts.append(placeholder)
    return " · ".join(parts)

def main():
    vocabulary=json.loads((ROOT/"tools/translation-vocabulary.json").read_text());keys=vocabulary["keys"].split("|")
    sources=sorted({m.findtext("source") for m in ET.parse(ROOT/"app/translations/harbour-plexfreq.ts").getroot().iter("message")})
    existing=json.loads((ROOT/"tools/translations.json").read_text()) if "--missing-only" in sys.argv else {}
    if "--preserve-committed" in sys.argv:
        import subprocess
        baseline=json.loads(subprocess.check_output(["git","show","HEAD:tools/translations.json"],cwd=ROOT))
        for language,table in baseline.items():
            table.update(existing.get(language,{}));existing[language]=table
    output={}
    for language in LANGUAGES:
        values=vocabulary[language].split("|")
        if len(values)!=len(keys):raise ValueError(f"{language}: {len(values)} terms, expected {len(keys)}")
        words=dict(zip(keys,values));old=reference(language);table={}
        for source in sources:
            if source=="Show all":
                table[source]=existing.get(language,{}).get(source) or SHOW_ALL[language]
                continue
            # These category labels have no equivalent in the older compact
            # vocabulary; keep their authored names explicit in every locale.
            genre_mood_style={
                "bg":("Жанр","Настроение","Стил"),"bn":("ধরন","মেজাজ","শৈলী"),"cs":("Žánr","Nálada","Styl"),"da":("Genre","Stemning","Stil"),"de":("Genre","Stimmung","Stil"),"el":("Είδος","Διάθεση","Στυλ"),"es":("Género","Estado de ánimo","Estilo"),"et":("Žanr","Meeleolu","Stiil"),"fi":("Genre","Tunnelma","Tyyli"),"fr":("Genre","Ambiance","Style"),"gu":("પ્રકાર","મનોભાવ","શૈલી"),"hi":("शैली","मनोभाव","अंदाज़"),"hu":("Műfaj","Hangulat","Stílus"),"it":("Genere","Atmosfera","Stile"),"kn":("ಪ್ರಕಾರ","ಮನೋಭಾವ","ಶೈಲಿ"),"lt":("Žanras","Nuotaika","Stilius"),"lv":("Žanrs","Noskaņa","Stils"),"ml":("വിഭാഗം","മനോഭാവം","ശൈലി"),"mr":("प्रकार","भावना","शैली"),"nb":("Sjanger","Stemning","Stil"),"nl":("Genre","Stemming","Stijl"),"pa":("ਕਿਸਮ","ਮਨੋਭਾਵ","ਸ਼ੈਲੀ"),"pl":("Gatunek","Nastrój","Styl"),"pt":("Género","Estado de espírito","Estilo"),"pt_BR":("Gênero","Humor","Estilo"),"ro":("Gen","Stare","Stil"),"ru":("Жанр","Настроение","Стиль"),"sk":("Žáner","Nálada","Štýl"),"sl":("Zvrst","Razpoloženje","Slog"),"sv":("Genre","Stämning","Stil"),"ta":("வகை","மனநிலை","பாணி"),"te":("రకం","మనోభావం","శైలి"),"tr":("Tür","Ruh hâli","Stil"),"tt":("Жанр","Кәеф","Стиль"),"uk":("Жанр","Настрій","Стиль"),"vi":("Thể loại","Tâm trạng","Phong cách"),"zh_CN":("流派","情绪","风格"),"zh_HK":("類型","情緒","風格"),"zh_TW":("類型","情緒","風格")
            }
            if source in ("Genre","Mood","Style"):
                table[source]=existing.get(language,{}).get(source) or genre_mood_style[language][("Genre","Mood","Style").index(source)]
                continue
            translated=existing.get(language,{}).get(source) or old.get(source) or compact(source,words)
            if language=="it" and source not in existing.get(language,{}):translated=ITALIAN.get(source,translated)
            table[source]=translated
        output[language]=table
    (ROOT/"tools/translations.json").write_text(json.dumps(output,ensure_ascii=False,indent=2)+"\n")
    print(f"Authored {len(LANGUAGES)} languages × {len(sources)} source messages")

if __name__=="__main__":main()
