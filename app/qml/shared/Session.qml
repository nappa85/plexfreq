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
    signal openFilterEditor()
    signal openDiscoverySettings()
    signal openInsights()
    property string filterAction:"browse"
    property string filterKey:""
    property string filterSection:""
    property string filterKind:"track"
    property string filterName:""
    property string filterSort:"title"
    property int filterLimit:100
    property bool filterEditable:true
    property bool filterLoading:false
    property var filterValues:({genre:"",mood:"",style:"",title:"",yearFrom:null,yearTo:null,minRating:null,unplayed:false})
    property var filterChoices:({genre:[],mood:[],style:[]})
    property var journeyTracks:[]
    function requestFilterChoices() {filterChoices={genre:[],mood:[],style:[]};if(filterKind==="artist"){filterValue("yearFrom",null);filterValue("yearTo",null)}if(filterSection)for(var i=0;i<3;i++)backend.command("filter_options",{section:filterSection,kind:filterKind,field:["genre","mood","style"][i]})}
    function filterSortText(value) {return value==="title" ? qsTr("Title") : value==="newest" ? qsTr("Recently added") : value==="year" ? qsTr("Year") : value==="played" ? qsTr("Recently played") : value==="popular" ? qsTr("Most played") : qsTr("Shuffle")}
    function editFilters(action,item) {
        filterAction=action;filterKey=item ? item.ratingKey : "";filterSection=section;filterKind=action==="browse" ? browseKind : "track";filterName=item ? item.title : "";filterSort="title";filterLimit=100;filterEditable=true;filterLoading=action==="update"
        filterValues={genre:"",mood:"",style:"",title:"",yearFrom:null,yearTo:null,minRating:null,unplayed:false}
        if(filterLoading)backend.command("smart_rules",{key:filterKey});else requestFilterChoices()
        openFilterEditor()
    }
    function filterValue(name,value) {var copy={};for(var key in filterValues)copy[key]=filterValues[key];copy[name]=value;filterValues=copy}
    function filterText(name) {var value=filterValues[name];return value===null || value===undefined ? "" : ""+value}
    function filterChoiceIndex(choices,key) {for(var i=0;i<choices.length;i++)if(choices[i].key===key)return i;return 0}
    function submitFilters() {
        if(filterAction==="browse")load("filtered_browse",{section:filterSection,kind:filterKind,sort:filterSort,filters:filterValues,start:0},qsTr("Filtered music"),true)
        else backend.command("smart_playlist",{action:filterAction,key:filterKey,section:filterSection,title:filterName,sort:filterSort,limit:filterLimit,filters:filterValues})
    }
    function toggleHub(identifier,hidden) {var layout=backend.state.discoveryLayout || ({hidden:[],order:[]});var list=(layout.hidden || []).filter(function(id){return id!==identifier});if(hidden)list.push(identifier);backend.command("discovery_config",{section:section,hidden:list,order:layout.order || []})}
    function moveHub(index,direction) {var options=backend.state.discoveryHubs || [];var order=options.map(function(h){return h.identifier});var target=index+direction;if(target<0 || target>=order.length)return;var id=order[index];order.splice(index,1);order.splice(target,0,id);backend.command("discovery_config",{section:section,hidden:(backend.state.discoveryLayout || {}).hidden || [],order:order})}
    function hub(item) {if(item && item.hubKey)load("hub_items",{key:item.hubKey,start:0},item.discoveryGroup || item.title,true)}
    function groupHub(title,entries) {
        var target=null
        for(var i=0;i<entries.length;i++) {
            var item=entries[i]
            if(item.discoveryGroup!==title || !item.hubKey)continue
            if(target && target.hubKey!==item.hubKey)return null
            target=item
        }
        return target
    }
    function discoveryGridItems(entries,index) {
        if(!entries || index<0 || index>=entries.length || !entries[index].discoveryGrid)return []
        var identifier=entries[index].hubIdentifier
        if(index>0 && entries[index-1].hubIdentifier===identifier)return []
        var result=[]
        for(var i=index;i<entries.length && entries[i].hubIdentifier===identifier;i++)result.push({entry:entries[i],sourceIndex:i})
        return result
    }
    function addJourneyTrack(item) {if(item && item.type==="track" && journeyTracks.length<8)journeyTracks=journeyTracks.concat([item])}
    function journey() {if(journeyTracks.length>=2)load("sonic_journey",{section:section,keys:journeyTracks.map(function(i){return i.ratingKey})},qsTr("Sonic Adventure"),true)}
    property int downloadMinutes:60
    property var adventureStart:null
    property bool homeView:!showQueue && route && route.op==="discovery_home"
    function home() {goHome(false)}
    function goHome(resetHistory) {if(section && !backend.busy){if(resetHistory)history=[];query="";load("discovery_home",{section:section},qsTr("Discover"),!resetHistory && route && route.op!=="discovery_home")}}
    function qualitySetting(name,value) {
        if (["wifiKbps","mobileKbps","downloadKbps","codecFallback"].indexOf(name)<0) return
        var old=backend.state.qualityConfig || ({wifiKbps:0,mobileKbps:0,downloadKbps:0,codecFallback:true})
        var config={wifiKbps:old.wifiKbps || 0,mobileKbps:old.mobileKbps || 0,downloadKbps:old.downloadKbps || 0,codecFallback:old.codecFallback!==false}
        config[name]=value;backend.command("quality_config",{config:config})
    }
    function qualityText(kbps) {return kbps ? qsTr("%1 kbps").arg(kbps) : qsTr("Original audio")}
    function downloadRadio(item) {
        if(item && item.station)backend.command("download_station",{key:item.key,title:item.title,minutes:downloadMinutes})
        else if(item && canRadio(item))backend.command("download_plan",{kind:item.type+"_radio",key:item.ratingKey,minutes:downloadMinutes})
    }
    function sonicNeighbors(item) {if(item)load("sonic_neighbors",{key:item.ratingKey,kind:item.type},qsTr("Sonically similar"),true)}
    function sonicAdventure(item) {if(adventureStart && item && section)load("sonic_adventure",{section:section,start_key:adventureStart.ratingKey,end_key:item.ratingKey},qsTr("Sonic Adventure"),true)}
    function audioSetting(name,value) {
        if (["crossfadeMs","normalization","eq","normalizationMode","headroomDb"].indexOf(name)<0) return
        var old=backend.state.audioConfig || ({crossfadeMs:0,normalization:false,eq:[0,0,0,0,0,0,0,0,0,0]})
        var config={crossfadeMs:old.crossfadeMs || 0,normalization:!!old.normalization,eq:old.eq || [0,0,0,0,0,0,0,0,0,0],normalizationMode:old.normalizationMode || "track",headroomDb:old.headroomDb || 0}
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
        if (!items || index<0 || index>=items.length || !playlist || !playlist.ratingKey) return
        if (down && index+1>=items.length) return
        if (!items[index] || !items[index].playlistItemId) return
        if (down && (!items[index+1] || !items[index+1].playlistItemId)) return
        var after=down ? items[index+1].playlistItemId : index>1 ? (items[index-1] && items[index-2] ? items[index-2].playlistItemId : null) : null
        if (!down && index===0) return
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
    function favorite(item) {if (!item || !item.ratingKey || backend.busy) return;backend.command("favorite",{key:item.ratingKey,enabled:!(item.userRating>=10)})}
    function discovery(view) {if (backend.busy || !section) return;query="";load("collection",{section:section,view:view,start:0},view==="favorites" ? qsTr("Favorites · 5 stars") : view==="added" ? qsTr("Recently added") : qsTr("Recently played"),true)}
    property var similarArtists: detail && detail.type === "artist" && detail.ratingKey === similarKey ? similarData : []
    property var items: (showQueue ? (backend.state.queue || {}).items : backend.state.items) || []
    property bool canGoBack: history.length > 0
    property var detail: !showQueue && route && (route.op === "detail" || route.op === "artist_albums") ?
        (detailData && detailData.ratingKey === route.args.key ? detailData : route.seed) : null
    property var playlist: !showQueue && route && route.seed && route.seed.type === "playlist" ? route.seed : null
    property var rows: showQueue ? backend.queueModel : backend.itemsModel
    property bool artistBrowse: !showQueue && route && route.op === "browse" && browseKind==="artist" && browseSort==="title"
    property bool gridBrowse: artistBrowse || (!showQueue && route && route.op === "collection" && (route.args.view === "added" || route.args.view === "played"))
    property var alphabet: artistBrowse && query.length === 0 && backend.state.alphabetSection === section ? backend.state.alphabet || [] : []
    signal navigate(string kind, string key)
    signal resetView()
    signal artistJumped(int index)
    signal beforeViewChange(string op, var args)

    function reset() {
        history = []; route = null; section = ""; query = ""; showQueue = false
        mixSeeds=[];editorItems=[];editorTarget=null
        adventureStart=null
        journeyTracks=[]
        heading = qsTr("Artists")
        detailData = null; failedPageStart = -1; requestedPageStart = -1; indexedSection = ""
        similarKey = ""; similarData = []; similarState = ""
    }
    function load(op, args, title, remember, seed) {
        if (backend.busy) return
        beforeViewChange(op, args)
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
    function browse() {browseLibrary(false)}
    function browseLibrary(remember) {
        if (!section || backend.busy) return
        // true enters Library, null refines it, false starts a fresh browse.
        if (remember === false) history = []
        var title=browseKind==="artist" ? qsTr("Artists") : browseKind==="album" ? qsTr("Albums") : qsTr("Tracks")
        if (backend.state.offlineMode) {load("offline_browse",{kind:browseKind},title,!!remember);return}
        if(browseKind!=="artist" || browseSort!=="title"){load("library_browse",{section:section,kind:browseKind,sort:browseSort,start:0},title,!!remember);return}
        load("browse", { section: section, kind: "artist", query: query, start: 0 }, qsTr("Artists"), !!remember)
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
        for(var i=0;i<alphabet.length;i++)if(alphabet[i].letter===letter){artistJumped(alphabet[i].offset || 0);return}
    }
    function activate(item, index) {
        if (!item || backend.busy || searchPending) return
        if(item.station) {if(item.key)backend.command("station",{key:item.key,title:item.title || ""});return}
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
        if (!item || !item.type || backend.busy) return
        if (item.type === "album") {if(item.ratingKey)backend.command("enqueue_album",{key:item.ratingKey,next:next})}
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
    function artist(item) { if(!item) return ""; return item.originalTitle || item.grandparentTitle || item.parentTitle || "" }
    function time(ms) { var s = Math.floor(ms / 1000); return Math.floor(s / 60) + ":" + (s % 60 < 10 ? "0" : "") + s % 60 }
    function totalTime(ms) {var s=Math.floor(ms/1000);var h=Math.floor(s/3600);var m=Math.floor(s/60)%60;return h>0 ? h+":"+(m<10 ? "0" : "")+m+":"+(s%60<10 ? "0" : "")+s%60 : time(ms)}
    function playlistSummary(item) {
        if(!item) return qsTr("Track count unavailable")
        var count=item.leafCount===null || item.leafCount===undefined ? qsTr("Track count unavailable") : item.leafCount===1 ? qsTr("1 track") : qsTr("%1 tracks").arg(item.leafCount)
        var duration=item.totalDuration===null || item.totalDuration===undefined ? qsTr("Duration unavailable") : totalTime(item.totalDuration)
        // Keep plain concatenation (no new catalogue string): count/duration
        // are already translated; order is fixed to avoid a new source key.
        return count+" · "+duration
    }
    function canRadio(item) { return item && item.ratingKey && (item.type === "artist" || item.type === "album" || item.type === "track") }
    function radioActionText(item) { if(!item || !item.type) return ""; return item.type === "artist" ? qsTr("Artist radio") : item.type === "album" ? qsTr("Album radio") : qsTr("Track radio") }
    function startRadio(item) {
        if (!backend.busy && canRadio(item)) backend.command("radio", {key:item.ratingKey, kind:item.type})
    }

    Timer {
        id: searchTimer; interval: 350; repeat: false
        onTriggered: {
            interval = 350
            if (backend.busy) { restart(); return }
            session.searchPending = false
            if (backend.state.offlineMode) session.load("offline_search",{query:query,start:0},qsTr("Offline library"),false)
            else if (query.length>0) session.load("search",{query:query},qsTr("Search results"),false)
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
            if(op==="filter_options" && data.filterSection===filterSection && data.filterKind===filterKind) {var choices={genre:filterChoices.genre,mood:filterChoices.mood,style:filterChoices.style};choices[data.filterField]=ok ? data.filterChoices || [] : [];filterChoices=choices;return}
            if(op==="smart_rules" && filterAction==="update" && data.smartKey===filterKey) {filterLoading=false;filterEditable=ok && !!data.smartEditable;if(filterEditable){filterSection=data.smartSection;filterKind="track";filterValues=data.smartFilters;filterSort=data.smartSort;filterLimit=data.smartLimit;requestFilterChoices()}return}
            if(op==="discovery_config" && ok){load("discovery_home",{section:section},qsTr("Discover"),false);return}
            if(op==="smart_playlist" && ok){load("playlists",{start:0},qsTr("Playlists"),false);return}
            if (op==="filtered_browse" || op==="hub_items" || op==="sonic_journey" || op==="discovery_home" || op==="sonic_neighbors" || op==="sonic_adventure" || op==="offline_search" || op==="library_browse" || op==="artist_albums" || op==="playlist_items" || op === "browse" || op === "search" || op === "collection" || op === "offline_browse" || op === "detail" || op === "children" || op === "playlists" || op === "jump_artist") {
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
                if (ok && op === "jump_artist") artistJumped(data.selectedIndex || 0)
                if (ok && op === "browse" && query.length === 0 && indexedSection !== section) {
                    indexedSection = section; backend.command("alphabet", {section:section})
                }
                if(ok && op==="browse" && artistBrowse && query.length===0 && backend.state.hasMore)Qt.callLater(more)
            }
            if(op==="playlist_edit" && ok) {
                if(playlist && data.playlistChanged===playlist.ratingKey && !data.playlistDeleted)load("playlist_items",{key:playlist.ratingKey,start:0},heading,false,playlist)
                else if((route && route.op==="playlists") || data.createdPlaylist || data.playlistDeleted)load("playlists",{start:0},qsTr("Playlists"),false)
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
                if (data.libraries && data.libraries.length > 0) { session.section = data.libraries[0].key; session.home() }
            } else if ((op === "radio" || op === "station" || op === "mix" || op === "play_download") && ok) { session.showQueue = true }
            else if (op === "logout" && ok) { session.polling = false; session.reset() }
        }
    }
}
