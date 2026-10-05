import QtQuick 2.6
import PlexFreq 1.0

// Page-local presentation only. Session/Rust still own navigation and data policy.
Item {
    id: view
    visible: false
    property var music
    property string pageKind: "root"
    property string pageKey: ""
    property bool frozen: false
    property var saved: ({})
    property string heading: frozen ? saved.heading || "" : music.heading
    property var detail: frozen ? saved.detail : music.detail
    property var playlist: frozen ? saved.playlist : music.playlist
    property var items: frozen ? saved.items || [] : music.items
    property var rows: entries
    property bool showQueue: frozen ? !!saved.showQueue : music.showQueue
    property bool homeView: frozen ? !!saved.homeView : music.homeView
    property bool artistBrowse: frozen ? !!saved.artistBrowse : music.artistBrowse
    property var alphabet: frozen ? saved.alphabet || [] : music.alphabet
    property var route: frozen ? saved.route : music.route
    property string query: frozen ? saved.query || "" : music.query
    property var similarArtists: frozen ? saved.similarArtists || [] : music.similarArtists
    property string similarState: frozen ? saved.similarState || "" : music.similarState
    property bool canGoBack: frozen ? !!saved.canGoBack : music.canGoBack
    property bool busy: frozen ? !!saved.busy : backend.busy
    property bool loadingMore: frozen ? !!saved.loadingMore : backend.loadingMore
    property bool hasMore: frozen ? !!saved.hasMore : !!backend.state.hasMore
    property bool offline: frozen ? !!saved.offline : !!backend.state.offline
    property string error: frozen ? saved.error || "" : backend.error || ""
    property int failedPageStart: frozen ? saved.failedPageStart : music.failedPageStart

    EntryModel { id: entries; entries: view.items }
    function freeze() {
        if (frozen) return
        saved = {heading:heading,detail:detail,playlist:playlist,items:items,
            showQueue:showQueue,homeView:homeView,artistBrowse:artistBrowse,
            alphabet:alphabet,route:route,query:query,similarArtists:similarArtists,
            similarState:similarState,canGoBack:canGoBack,busy:busy,
            loadingMore:loadingMore,hasMore:hasMore,offline:offline,error:error,
            failedPageStart:failedPageStart}
        frozen = true
    }
    function thaw() { frozen = false }
    Connections {
        target: music
        onBeforeViewChange: {
            if (!music.route || !music.matchesPage(view.pageKind, view.pageKey)) return
            var isDetail = op === "detail" || op === "artist_albums"
            if (view.pageKind === "root" ? isDetail : !isDetail || args.key !== view.pageKey) view.freeze()
        }
    }
}
