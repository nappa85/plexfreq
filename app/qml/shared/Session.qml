import QtQuick 2.6

Item {
    id: session
    visible: false
    property string section: ""
    property string query: ""
    property string heading: qsTr("Artists")
    property string browseKind:"artist"
    property string browseSort:"title"
    property var mixSeeds:[]
    property string editorAction:"create"
    property var editorTarget:null
    property var editorItems:[]
    signal editPlaylistRequested()
    signal openDownloads()
    signal openAudioSettings()
    function audioSetting(name,value) {
        var old=backend.state.audioConfig || ({crossfadeMs:0,normalization:false,eq:[0,0,0,0,0,0,0,0,0,0]})
        var config={crossfadeMs:old.crossfadeMs || 0,normalization:!!old.normalization,eq:old.eq || [0,0,0,0,0,0,0,0,0,0]}
        config[name]=value;backend.command("audio_config",{config:config})
    }
    function playlistEditor(action,item) {
        editorAction=action;editorTarget=action==="rename" ? item : null
        editorItems=item && item.type!=="playlist" ? [item] : items.filter(function(i){return i.type==="track"})
        if(action==="create" && editorItems.length===0)editorItems=(backend.state.queue || {}).items || []
        if(action==="add")backend.command("playlist_choices")
        editPlaylistRequested()
    }
    function submitPlaylist(key,title) {backend.command("playlist_edit",{action:editorAction,key:key || "",title:title || "",items:editorItems})}
    function playlistMove(index,down) {
        var after=down ? items[index+1].playlistItemId : index>1 ? items[index-2].playlistItemId : null
        backend.command("playlist_edit",{action:"move",key:playlist.ratingKey,item_id:items[index].playlistItemId,after:after})
    }
    function addMixSeed(item) {if(mixSeeds.length<10 && !mixSeeds.some(function(i){return i.ratingKey===item.ratingKey}))mixSeeds=mixSeeds.concat([item])}
    function playMix() {if(!backend.busy && mixSeeds.length)backend.command("mix",{seeds:mixSeeds})}
    function groupAlbums() {if(detail && detail.type==="artist")load("artist_albums",{key:detail.ratingKey},heading,false,detail)}
    property var route: null
    property var history: []
    property bool showQueue: false
    property bool polling: false
    property string loginUrl: ""
    property int catalogCount: -1
    property var detailData: null
    property int failedPageStart: -1
    property int requestedPageStart: -1
    onRouteChanged: requestedPageStart = -1
    property string indexedSection: ""
    property bool searchPending: false
    property string similarKey: ""
    property var similarData: []
    property string similarState: ""
    property bool nowPlaying: false
    property string trackKey: backend.state.track ? backend.state.track.ratingKey || "" : ""
    property var lyrics: []
    property string lyricsState: ""
    onTrackKeyChanged: {lyrics=[];lyricsState="";if(nowPlaying)requestLyrics()}
    onNowPlayingChanged: if (nowPlaying && lyricsState === "") requestLyrics()
    signal openPlayer()
    function requestLyrics() {if(trackKey){lyricsState="loading";backend.command("lyrics",{key:trackKey})}}
    function lyricIndex() {var found=-1;for(var i=0;i<lyrics.length;i++)if(lyrics[i].time!==null && lyrics[i].time<=backend.position)found=i;return found}
    function favorite(item) {if (!backend.busy)backend.command("favorite",{key:item.ratingKey,enabled:!(item.userRating>=10)})}
    function discovery(view) {query="";load("collection",{section:section,view:view,start:0},view==="favorites" ? qsTr("Favorites · 5 stars") : view==="added" ? qsTr("Recently added") : qsTr("Recently played"),true)}
    property var similarArtists: detail && detail.type === "artist" && detail.ratingKey === similarKey ? similarData : []
    property var items: (showQueue ? (backend.state.queue || {}).items : backend.state.items) || []
    property bool canGoBack: history.length > 0
    property var detail: !showQueue && route && (route.op === "detail" || route.op === "artist_albums") ?
        (detailData && detailData.ratingKey === route.args.key ? detailData : route.seed) : null
    property var playlist: !showQueue && route && route.seed && route.seed.type === "playlist" ? route.seed : null
    property var rows: showQueue ? backend.queueModel : backend.itemsModel
    property bool artistBrowse: !showQueue && route && route.op === "browse" && browseKind==="artist" && browseSort==="title"
    property var alphabet: artistBrowse && query.length === 0 && backend.state.alphabetSection === section ? backend.state.alphabet || [] : []
    signal navigate(string kind, string key)
    signal resetView()

    function reset() {
        history = []; route = null; section = ""; query = ""; showQueue = false
        mixSeeds=[];editorItems=[];editorTarget=null
        heading = qsTr("Artists")
        detailData = null; failedPageStart = -1; requestedPageStart = -1; indexedSection = ""
        similarKey = ""; similarData = []; similarState = ""
    }
    function load(op, args, title, remember, seed) {
        if (backend.busy) return
        if (op !== "browse") { searchTimer.stop(); searchPending = false }
        else if (args.query !== undefined) query = args.query
        if (remember && route) history = history.concat([route])
        showQueue = false
        heading = title
        route = { op: op, args: args, title: title, seed: seed || null }
        similarKey = ""; similarData = []
        similarState = op === "detail" && seed && seed.type === "artist" ? "loading" : ""
        failedPageStart = -1; requestedPageStart = -1
        backend.command(op, args)
    }
    function browse() {
        if (!section || backend.busy) return
        history = []
        var title=browseKind==="artist" ? qsTr("Artists") : browseKind==="album" ? qsTr("Albums") : qsTr("Tracks")
        if (backend.state.offlineMode) {load("offline_browse",{kind:browseKind},title,false);return}
        if(browseKind!=="artist" || browseSort!=="title"){load("library_browse",{section:section,kind:browseKind,sort:browseSort,start:0},title,false);return}
        load("browse", { section: section, kind: "artist", query: query, start: 0 }, qsTr("Artists"), false)
    }
    function search(value) {
        if (value === query) return
        query = value; searchPending = true; searchTimer.restart()
    }
    function submitSearch(value) {
        query = value; searchPending = true; searchTimer.interval = 1; searchTimer.restart()
    }
    function jump(letter) {
        if (backend.busy || !section) return
        searchTimer.stop(); searchPending = false; query = ""; history = []
        var offset = 0
        for (var i = 0; i < alphabet.length; i++) if (alphabet[i].letter === letter) offset = alphabet[i].offset
        route = {op:"browse", args:{section:section, kind:"artist", query:"", start:offset, _replace:true}, title:qsTr("Artists"), seed:null}
        heading = qsTr("Artists"); showQueue = false; failedPageStart = -1; requestedPageStart = -1
        backend.command("jump_artist", {section:section, letter:letter})
    }
    function activate(item, index) {
        if (backend.busy || searchPending) return
        if (showQueue) backend.command("select_track", { index: index })
        else if (item.type === "track") {
            var tracks = []; var selected = 0
            for (var i = 0; i < items.length; i++) {
                if (items[i].type === "track") {
                    if (i === index) selected = tracks.length
                    tracks.push(items[i])
                }
            }
            backend.command("play", { items: tracks, index: selected })
        } else if (item.type === "artist" || item.type === "album") {
            load("detail", {key:item.ratingKey, start:0}, item.title, true, item)
            navigate(item.type, item.ratingKey)
        } else if(item.type==="playlist")load("playlist_items",{key:item.ratingKey,start:0},item.title,true,item)
        else load("children", { key: item.key, start: 0 }, item.title, true, item)
    }
    function back() {
        if (!canGoBack || backend.busy) return
        var copy = history.slice(); var previous = copy.pop(); history = copy
        load(previous.op, previous.args, previous.title, false, previous.seed)
    }
    function matchesPage(kind, key) {
        if (!route) return true
        var isDetail=route.op==="detail" || route.op==="artist_albums"
        return kind === "root" ? !isDetail : isDetail && route.args.key === key
    }
    function playAll() {
        if (backend.busy) return
        if (detail && detail.type === "album") {backend.command("play_album",{key:detail.ratingKey});return}
        var tracks = []; for (var i = 0; i < items.length; i++) if (items[i].type === "track") tracks.push(items[i])
        if (tracks.length) backend.command("play", {items:tracks, index:0})
    }
    function enqueue(item,next) {
        if (backend.busy) return
        if (item.type === "album") backend.command("enqueue_album",{key:item.ratingKey,next:next})
        else if (item.type === "track") backend.command("enqueue",{items:[item],next:next})
    }
    function moveQueue(from,to) {if (!backend.busy) backend.command("queue_move",{from:from,to:to})}
    function downloads() { load("cached_tracks", {}, qsTr("Downloaded music"), route && route.op !== "cached_tracks") }
    function downloadTracks() {
        if (backend.busy) return
        var tracks = []; for (var i = 0; i < items.length; i++) if (items[i].type === "track") tracks.push(items[i])
        if (tracks.length) backend.command("cache_tracks", {items:tracks})
    }
    function more() {
        if (!route || backend.busy || backend.loadingMore || showQueue || !backend.state.hasMore || searchPending) return
        if (failedPageStart === backend.state.next || requestedPageStart === backend.state.next) return
        var args = {}; for (var key in route.args) args[key] = route.args[key]
        args.start = backend.state.next
        delete args._replace
        requestedPageStart = args.start
        backend.command(route.op, args)
    }
    function maybeMore(fraction) { if (fraction >= 0.75) more() }
    function retryMore() { failedPageStart = -1; requestedPageStart = -1; more() }
    function artist(item) { return item.originalTitle || item.grandparentTitle || item.parentTitle || "" }
    function time(ms) { var s = Math.floor(ms / 1000); return Math.floor(s / 60) + ":" + (s % 60 < 10 ? "0" : "") + s % 60 }
    function totalTime(ms) {var s=Math.floor(ms/1000);var h=Math.floor(s/3600);var m=Math.floor(s/60)%60;return h>0 ? h+":"+(m<10 ? "0" : "")+m+":"+(s%60<10 ? "0" : "")+s%60 : time(ms)}
    function playlistSummary(item) {
        var count=item.leafCount===null || item.leafCount===undefined ? qsTr("Track count unavailable") : item.leafCount===1 ? qsTr("1 track") : qsTr("%1 tracks").arg(item.leafCount)
        return count+" · "+(item.totalDuration===null || item.totalDuration===undefined ? qsTr("Duration unavailable") : totalTime(item.totalDuration))
    }
    function canRadio(item) { return item && item.ratingKey && (item.type === "artist" || item.type === "album" || item.type === "track") }
    function radioActionText(item) { return item.type === "artist" ? qsTr("Artist radio") : item.type === "album" ? qsTr("Album radio") : qsTr("Track radio") }
    function startRadio(item) {
        if (!backend.busy && canRadio(item)) backend.command("radio", {key:item.ratingKey, kind:item.type})
    }

    Timer {
        id: searchTimer; interval: 350; repeat: false
        onTriggered: {
            interval = 350
            if (backend.busy) { restart(); return }
            session.searchPending = false
            if (query.length>0) session.load("search",{query:query},qsTr("Search results"),false)
            else session.browse()
        }
    }
    Timer {
        interval: 2000; repeat: true; running: session.polling
        onTriggered: if (!backend.busy) backend.command("poll_login")
    }
    Connections {
        target: backend
        onCompleted: {
            if (data._discarded) return
            if (op==="library_browse" || op==="artist_albums" || op==="playlist_items" || op === "browse" || op === "search" || op === "collection" || op === "offline_browse" || op === "detail" || op === "children" || op === "playlists" || op === "jump_artist") {
                if (requestedPageStart >= 0 && data._pageStart === requestedPageStart) {
                    if (!ok) failedPageStart = requestedPageStart
                    requestedPageStart = -1
                }
                if(ok && data.playlist && route && route.op==="playlist_items") {
                    route={op:route.op,args:route.args,title:data.playlist.title,seed:data.playlist};heading=data.playlist.title
                }
                if (ok && data.detail) {
                    detailData = data.detail
                    if (data.detail.type === "artist") {
                        similarKey = data.detail.ratingKey; similarData = []; similarState = "loading"
                        backend.command("similar_artists", {key:similarKey})
                    } else { similarKey = ""; similarData = []; similarState = "" }
                }
                if (ok && (data.start === 0 || data.replaceItems)) resetView()
                if (ok && op === "browse" && query.length === 0 && indexedSection !== section) {
                    indexedSection = section; backend.command("alphabet", {section:section})
                }
            }
            if(op==="playlist_edit" && ok) {
                if(playlist && data.playlistChanged===playlist.ratingKey && !data.playlistDeleted)load("playlist_items",{key:playlist.ratingKey,start:0},heading,false,playlist)
                else if(route && route.op==="playlists" || data.createdPlaylist || data.playlistDeleted)load("playlists",{start:0},qsTr("Playlists"),false)
            }
            if (op === "lyrics" && data.lyricsKey === trackKey) {lyrics=ok ? data.lyrics || [] : [];lyricsState=ok ? "ready" : "unavailable";return}
            if (op === "offline_mode" && ok) {query="";browse();return}
            if (op === "similar_artists") {
                if (ok && detail && detail.type === "artist" && data.similarKey === detail.ratingKey) {
                    similarKey = data.similarKey; similarData = data.similarArtists || []; similarState = data.similarAvailable ? "ready" : "unavailable"
                } else if (!ok && detail && data.similarKey === detail.ratingKey) similarState = "unavailable"
                return
            }
            if (op === "login" && ok) {
                session.loginUrl = data.url; session.polling = true
                Qt.openUrlExternally(data.url)
            } else if (op === "poll_login") {
                if (!ok || data.signedIn) session.polling = false
                if (ok && data.signedIn) backend.command("servers")
            } else if (op === "status" && ok) {
                if (data.serverUrl) backend.command("libraries")
                else if (data.signedIn) backend.command("servers")
            } else if (op === "libraries" && !ok && backend.cache.tracks > 0) {
                session.downloads()
            } else if (op === "cached_tracks" && ok) {
                session.catalogCount = data.items.length
            } else if (op === "cache_status" && ok && !session.showQueue && session.route && session.route.op === "cached_tracks" && session.catalogCount !== backend.cache.tracks) {
                backend.command("cached_tracks", {_background:true, _refresh:true})
            } else if ((op === "connect" || op === "select_server" || op === "libraries") && ok) {
                session.polling = false
                session.reset()
                if (data.libraries && data.libraries.length > 0) { session.section = data.libraries[0].key; session.browse() }
            } else if ((op === "radio" || op === "mix") && ok) { session.showQueue = true }
            else if (op === "logout" && ok) { session.polling = false; session.reset() }
        }
    }
}
