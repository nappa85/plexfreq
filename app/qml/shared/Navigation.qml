import QtQuick 2.6

// Shared presentation/action catalogue. Native adapters only render these IDs
// and handle window/page-stack requests; Session and Rust retain their policies.
Item {
    id: navigation
    visible: false
    property var music
    property var view: music
    property bool interactive: true
    property bool rootNavigation: true
    property bool ready: interactive && !!music && !!view && !backend.busy
    property bool libraryView: !!view && !view.showQueue && !view.detail && !!view.route &&
        ["browse", "library_browse", "offline_browse", "filtered_browse"].indexOf(view.route.op) >= 0
    property string currentDestination: destination()
    property bool offlineEnabled: !!backend.state.offlineMode
    property string offlineLabel: qsTr("Offline library")
    property string actionsTitle: qsTr("Page actions")
    property string destinationsTitle: qsTr("More destinations")
    property string showAllLabel: qsTr("Show all")

    // Stable ID models keep native controls alive when labels/capabilities change.
    property var primaryDestinations: ["discover", "library", "playlists"]
    property var pageActions: ["customize", "filters", "groupAlbums", "createSmart", "createPlaylist",
        "downloadTracks", "playMix", "clearMix", "journey", "clearJourney", "stopRadio"]
    property var secondaryDestinations: ["queue", "downloaded", "savedAlbums", "insights",
        "favorites", "added", "played", "downloads", "connection"]
    property var labels: ({
        discover: qsTr("Discover"), library: qsTr("Library"), playlists: qsTr("Playlists"),
        customize: qsTr("Customize discovery"), filters: qsTr("Library filters"),
        groupAlbums: qsTr("Group albums by type"), createSmart: qsTr("Create smart playlist"),
        createPlaylist: qsTr("Create playlist from tracks/queue"),
        downloadTracks: qsTr("Download displayed tracks"), clearMix: qsTr("Clear mix seeds"),
        clearJourney: qsTr("Clear sonic waypoints"), stopRadio: qsTr("Stop radio"),
        queue: qsTr("Queue"), downloaded: qsTr("Downloaded music"), savedAlbums: qsTr("Saved albums"),
        insights: qsTr("Listening insights"), favorites: qsTr("Favorites · 5 stars"),
        added: qsTr("Recently added"), played: qsTr("Recently played"),
        downloads: qsTr("Download manager"), connection: qsTr("Connection"), back: qsTr("Back"),
        offline: offlineEnabled ? qsTr("Go online") : qsTr("Browse saved library offline"),
        playMix: qsTr("Play mix (%1 seeds)").arg(music ? music.mixSeeds.length : 0),
        journey: qsTr("Sonic journey (%1 tracks)").arg(music ? music.journeyTracks.length : 0)
    })
    signal connectionRequested()
    signal backRequested()

    function destination() {
        if (!music || !view || view.showQueue) return ""
        var routes=music.history.concat(view.route ? [view.route] : [])
        for (var i=routes.length-1;i>=0;i--) {
            var op=routes[i].op
            if (["discovery_home", "collection", "hub_items"].indexOf(op)>=0) return "discover"
            if (["browse", "library_browse", "offline_browse", "filtered_browse", "search", "offline_search"].indexOf(op)>=0) return "library"
            if (op==="playlists" || op==="playlist_items") return "playlists"
        }
        return ""
    }
    function definition(id) {
        if (labels[id]===undefined) return null
        var action={id:id,text:labels[id],visible:true,enabled:ready,
            selected:currentDestination===id,objectName:id+"Destination"}
        if (primaryDestinations.indexOf(id)>=0 || ["downloaded", "savedAlbums", "favorites", "added", "played"].indexOf(id)>=0)
            action.visible=rootNavigation
        var section=music && !!music.section
        var server=!!backend.state.serverUrl
        var detail=view ? view.detail : null
        var items=view ? view.items : []
        switch (id) {
        case "discover": case "library": action.enabled=ready && section; break
        case "playlists": action.enabled=ready && server; break
        case "customize": action.visible=!!view && !!view.homeView; break
        case "filters": action.visible=libraryView; action.enabled=ready && section; break
        case "groupAlbums": action.visible=!!detail && detail.type==="artist"; break
        case "createSmart": action.enabled=ready && section && !offlineEnabled; break
        case "createPlaylist": action.enabled=ready && server && !offlineEnabled; break
        case "downloadTracks":
            var hasTrack=false
            for (var ti=0;ti<items.length;ti++) if (items[ti] && items[ti].type==="track") {hasTrack=true;break}
            action.visible=!!view && !detail && !view.showQueue && hasTrack
            action.enabled=ready && !!backend.cache.enabled
            break
        case "playMix": action.visible=!!music && music.mixSeeds.length>0; break
        case "clearMix": action.visible=!!music && music.mixSeeds.length>0; action.enabled=interactive && !!music; break
        case "journey": action.visible=!!music && music.journeyTracks.length>=2; break
        case "clearJourney": action.visible=!!music && music.journeyTracks.length>0; action.enabled=interactive && !!music; break
        case "stopRadio": action.visible=!!backend.state.radio; break
        case "queue": action.enabled=interactive && !!music; break
        case "downloaded": action.enabled=ready && backend.cache.tracks>0; break
        case "savedAlbums": action.visible=rootNavigation && offlineEnabled; break
        case "insights": case "favorites": case "added": case "played": action.enabled=ready && section; break
        case "downloads": case "connection": action.enabled=interactive && !!music; break
        case "back":
            action.visible=!!view && (view.showQueue || (view.canGoBack && !view.homeView && !libraryView && (!view.route || view.route.op!=="playlists")))
            break
        }
        return action
    }
    function setOffline(enabled) {
        if (ready && enabled!==offlineEnabled) backend.command("offline_mode",{enabled:enabled})
    }
    function activate(id) {
        var action=definition(id)
        if (!action) {console.warn("unknown navigation action: " + id);return}
        if (!action.visible || !action.enabled) return
        switch (id) {
        case "discover": music.goHome(true); break
        case "library": if (!ready) return; music.query=""; music.browseLibrary(libraryView ? null : true); break
        case "playlists": music.load("playlists",{start:0},action.text,music.route && music.route.op!=="playlists"); break
        case "customize": music.openDiscoverySettings(); break
        case "filters": music.editFilters("browse",null); break
        case "groupAlbums": music.groupAlbums(); break
        case "createSmart": music.editFilters("create",null); break
        case "createPlaylist": music.playlistEditor("create",null); break
        case "downloadTracks": music.downloadTracks(); break
        case "playMix": music.playMix(); break
        case "clearMix": music.mixSeeds=[]; break
        case "journey": music.journey(); break
        case "clearJourney": music.journeyTracks=[]; break
        case "stopRadio": backend.command("stop_radio"); break
        case "queue": music.showQueue=true; break
        case "downloaded": music.downloads(); break
        case "savedAlbums": music.load("offline_browse",{kind:"album"},action.text,true); break
        case "insights": music.openInsights(); break
        case "favorites": case "added": case "played": music.discovery(id); break
        case "downloads": music.openDownloads(); break
        case "connection": connectionRequested(); break
        case "offline": setOffline(!offlineEnabled); break
        case "back": if (music.showQueue) music.showQueue=false; else backRequested(); break
        }
    }
}
