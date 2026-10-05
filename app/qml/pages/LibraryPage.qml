import QtQuick 2.6
import Sailfish.Silica 1.0
import "../shared"

Page {
    id: page
    property var music
    property string pageKind: "root"
    property string pageKey: ""
    property bool restoringView: false
    BrowsePageView { id: pageView; objectName:"browsePageView"; music: page.music; pageKind: page.pageKind; pageKey: page.pageKey }
    allowedOrientations: Orientation.All
    function restorePage() {
        if (status !== PageStatus.Active || !music || backend.busy) return
        if (pageKind !== "root" && (!music.route || music.route.op !== "detail" && music.route.op!=="artist_albums")) {
            pageStack.pop()
        } else if (!music.matchesPage(pageKind, pageKey) && music.canGoBack) {
            restoringView = true
            music.back()
        } else if (!restoringView) pageView.thaw()
    }
    onStatusChanged: {
        if (status === PageStatus.Deactivating) pageView.freeze()
        restorePage()
    }
    Connections { target: backend; onCompleted: {
        if (page.restoringView && !data._discarded && music.route && op === music.route.op && music.matchesPage(pageKind, pageKey)) {
            page.restoringView = false
            pageView.thaw()
        }
        page.restorePage()
    } }
    Connections { target: music; onResetView: if (page.status===PageStatus.Active) list.positionViewAtBeginning() }
    SilicaListView {
        id: list; anchors.fill: parent; anchors.bottomMargin: player.height; anchors.rightMargin:alphabet.visible ? alphabet.width : 0; clip: true
        model: pageView.rows
        headerPositioning:pageKind==="root" && pageView.artistBrowse ? ListView.PullBackHeader : ListView.InlineHeader
        function checkMore() { if (page.status===PageStatus.Active) music.maybeMore(visibleArea.yPosition+visibleArea.heightRatio) }
        onContentYChanged:moreTimer.restart()
        onCountChanged:moreTimer.restart()
        onHeightChanged:moreTimer.restart()
        Timer { id:moreTimer; interval:100; onTriggered:list.checkMore() }
        Connections { target:backend; onLoadingMoreChanged:if (!backend.loadingMore) moreTimer.restart() }
        PullDownMenu {
            MenuItem {text:qsTr("Discovery home");visible:pageKind==="root";enabled:!backend.busy;onClicked:music.home()}
            MenuItem { text: qsTr("Connection"); onClicked: pageStack.push(Qt.resolvedUrl("SettingsPage.qml"), {music: music}) }
            MenuItem {text:qsTr("Download manager");onClicked:music.openDownloads()}
            MenuItem {text:qsTr("Create playlist from tracks/queue");enabled:!backend.busy && !backend.state.offlineMode;onClicked:music.playlistEditor("create",null)}
            MenuItem {text:qsTr("Play mix (%1 seeds)").arg(music.mixSeeds.length);visible:music.mixSeeds.length>0;enabled:!backend.busy;onClicked:music.playMix()}
            MenuItem {text:qsTr("Clear mix seeds");visible:music.mixSeeds.length>0;onClicked:music.mixSeeds=[]}
            MenuItem { text: qsTr("Downloaded music"); enabled: !backend.busy && backend.cache.tracks > 0; onClicked: music.downloads() }
            MenuItem {text:backend.state.offlineMode?qsTr("Go online"):qsTr("Browse saved library offline");enabled:!backend.busy;onClicked:backend.command("offline_mode",{enabled:!backend.state.offlineMode})}
            MenuItem {text:qsTr("Saved albums");visible:!!backend.state.offlineMode;enabled:!backend.busy;onClicked:music.load("offline_browse",{kind:"album"},qsTr("Saved albums"),true)}
            MenuItem { text: qsTr("Download displayed tracks"); visible: music.items.length > 0 && music.items[0].type === "track"; enabled: !backend.busy && backend.cache.enabled; onClicked: music.downloadTracks() }
            MenuItem { text: qsTr("Stop radio"); visible: !!backend.state.radio; enabled: !backend.busy; onClicked: backend.command("stop_radio") }
            MenuItem { text: music.showQueue ? qsTr("Library") : qsTr("Queue"); onClicked: music.showQueue = !music.showQueue }
            MenuItem { text: qsTr("Playlists"); visible: pageKind === "root"; enabled: !backend.busy && !!backend.state.serverUrl; onClicked: music.load("playlists", {start:0}, qsTr("Playlists"), true) }
            MenuItem {text:qsTr("Favorites · 5 stars");visible:pageKind==="root";enabled:!backend.busy;onClicked:music.discovery("favorites")}
            MenuItem {text:qsTr("Recently added");visible:pageKind==="root";enabled:!backend.busy;onClicked:music.discovery("added")}
            MenuItem {text:qsTr("Recently played");visible:pageKind==="root";enabled:!backend.busy;onClicked:music.discovery("played")}
            MenuItem { text: qsTr("Back"); visible: music.canGoBack; enabled: !backend.busy; onClicked: pageStack.depth > 1 ? pageStack.pop() : music.back() }
        }
        header: Column {
            width: list.width
            height: implicitHeight
            PageHeader { title: pageView.showQueue ? qsTr("Up next") : pageKind === "root" ? "PlexFreq" : pageView.heading; description: pageKind === "root" && !pageView.showQueue ? pageView.heading : "" }
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;visible:!!pageView.playlist;text:pageView.playlist ? music.playlistSummary(pageView.playlist) : "";wrapMode:Text.Wrap;color:Theme.secondaryColor}
            Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; visible: !!backend.state.radio; text: backend.state.radio ? qsTr("Radio") + " · " + backend.state.radio.title : ""; color: Theme.highlightColor; wrapMode: Text.Wrap }
            Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; text: pageView.error; visible: text.length > 0; color: Theme.errorColor; wrapMode: Text.Wrap }
            Label {x:Theme.horizontalPageMargin;width:parent.width-2*x;visible:pageView.offline;text:qsTr("Offline · showing saved library metadata");wrapMode:Text.Wrap;color:Theme.secondaryColor}
            Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; text: backend.cache.downloading ? qsTr("Caching audio…") : qsTr("%1 tracks ready offline").arg(backend.cache.tracks); visible: backend.cache.downloading || backend.cache.tracks > 0; color: Theme.secondaryColor; font.pixelSize: Theme.fontSizeExtraSmall }
            ComboBox {
                width: parent.width; label: qsTr("Library"); enabled: !backend.busy; visible: pageKind === "root" && !pageView.showQueue && backend.state.libraries.length > 1
                menu: ContextMenu {
                    Repeater {
                        model: backend.state.libraries
                        MenuItem { text: modelData.title; onClicked: { music.section = modelData.key; music.browse() } }
                    }
                }
            }
            SearchField {
                id:artistSearch
                width: parent.width; visible: pageKind === "root" && !pageView.showQueue && !!backend.state.serverUrl && pageView.route && (pageView.route.op === "browse" || pageView.route.op === "library_browse" || pageView.route.op === "search" || pageView.route.op==="discovery_home" || pageView.route.op==="offline_browse" || pageView.route.op==="offline_search" || pageView.route.op==="cached_tracks")
                placeholderText: qsTr("Search artists, albums and tracks")
                text:pageView.query
                onTextChanged:if(!pageView.frozen)music.search(text)
                EnterKey.enabled: !backend.busy
                EnterKey.onClicked: { music.submitSearch(text); focus = false }
            }
            ComboBox {width:parent.width;label:qsTr("Browse");visible:pageKind==="root" && !pageView.showQueue && !pageView.detail && !pageView.playlist;value:music.browseKind==="artist"?qsTr("Artists"):music.browseKind==="album"?qsTr("Albums"):qsTr("Tracks")
                menu:ContextMenu {Repeater {model:["artist","album","track"];MenuItem {text:modelData==="artist"?qsTr("Artists"):modelData==="album"?qsTr("Albums"):qsTr("Tracks");onClicked:{music.browseKind=modelData;if(modelData==="artist" && music.browseSort==="year")music.browseSort="title";music.query="";music.browse()}}}}
            }
            ComboBox {width:parent.width;label:qsTr("Sort");visible:pageKind==="root" && !pageView.showQueue && !pageView.detail && !pageView.playlist;value:music.browseSort==="title"?qsTr("Title"):music.browseSort==="newest"?qsTr("Recently added"):qsTr("Year")
                menu:ContextMenu {Repeater {model:music.browseKind==="artist"?["title","newest"]:["title","newest","year"];MenuItem {text:modelData==="title"?qsTr("Title"):modelData==="newest"?qsTr("Recently added"):qsTr("Year");onClicked:{music.browseSort=modelData;music.query="";music.browse()}}}}
            }
            Button {anchors.horizontalCenter:parent.horizontalCenter;visible:pageView.detail && pageView.detail.type==="artist";text:qsTr("Group albums by type");enabled:!backend.busy;onClicked:music.groupAlbums()}
            BusyIndicator { anchors.horizontalCenter: parent.horizontalCenter; running: pageView.busy; visible: running; size: BusyIndicatorSize.Medium }
            IntrinsicLoader {
                width: parent.width; active: !!pageView.detail
                sourceComponent: Component { DetailHeader { width: list.width; music: page.music; view:pageView } }
            }
        }
        section.property:pageView.homeView ? "groupTitle" : pageView.route && pageView.route.op==="artist_albums" ? "albumGroup" : ""
        section.delegate:SectionHeader {text:section}
        delegate: ListItem {
            property var itemData: entry
            width: list.width
            contentHeight: Math.max(Theme.itemSizeMedium, rowDetails.height + 2 * Theme.paddingSmall, radioAction.height + 2 * Theme.paddingSmall)
            enabled: !backend.busy && page.status === PageStatus.Active
            menu: ContextMenu {
                MenuItem {text:qsTr("Sonically similar");visible:music.canRadio(itemData);onClicked:music.sonicNeighbors(itemData)}
                MenuItem {text:qsTr("Start sonic adventure here");visible:itemData.type==="track";onClicked:music.adventureStart=itemData}
                MenuItem {text:qsTr("Sonic Adventure to this track");visible:itemData.type==="track" && !!music.adventureStart;onClicked:music.sonicAdventure(itemData)}
                MenuItem {text:qsTr("Download radio · %1 minutes").arg(music.downloadMinutes);visible:itemData.station || music.canRadio(itemData);enabled:!backend.state.offlineMode;onClicked:music.downloadRadio(itemData)}
                MenuItem {text:qsTr("Download %1 minutes").arg(music.downloadMinutes);visible:itemData.type==="playlist" && !itemData.station;enabled:!backend.state.offlineMode;onClicked:backend.command("download_plan",{kind:"playlist",key:itemData.ratingKey,minutes:music.downloadMinutes})}
                MenuItem {text:qsTr("Play next"); visible:!music.showQueue && (itemData.type==="track" || itemData.type==="album"); onClicked:music.enqueue(itemData,true)}
                MenuItem {text:qsTr("Add to queue"); visible:!music.showQueue && (itemData.type==="track" || itemData.type==="album"); onClicked:music.enqueue(itemData,false)}
                MenuItem {text:qsTr("Add to playlist");visible:itemData.type==="track" || itemData.type==="album";enabled:!backend.state.offlineMode;onClicked:music.playlistEditor("add",itemData)}
                MenuItem {text:qsTr("Create playlist from this");visible:itemData.type==="track" || itemData.type==="album";enabled:!backend.state.offlineMode;onClicked:music.playlistEditor("create",itemData)}
                MenuItem {text:qsTr("Add as mix seed");visible:itemData.type==="artist" || itemData.type==="album";onClicked:music.addMixSeed(itemData)}
                MenuItem {text:qsTr("Rename playlist");visible:itemData.type==="playlist" && !itemData.station;onClicked:music.playlistEditor("rename",itemData)}
                MenuItem {text:qsTr("Delete playlist");visible:itemData.type==="playlist" && !itemData.station;onClicked:backend.command("playlist_edit",{action:"delete",key:itemData.ratingKey})}
                MenuItem {text:qsTr("Move playlist entry up");visible:!!music.playlist && !music.playlist.smart;enabled:index>0;onClicked:music.playlistMove(index,false)}
                MenuItem {text:qsTr("Move playlist entry down");visible:!!music.playlist && !music.playlist.smart;enabled:index+1<music.items.length;onClicked:music.playlistMove(index,true)}
                MenuItem {text:qsTr("Remove from playlist");visible:!!music.playlist && !music.playlist.smart;onClicked:backend.command("playlist_edit",{action:"remove",key:music.playlist.ratingKey,item_id:itemData.playlistItemId})}
                MenuItem {text:itemData.userRating>=10?qsTr("Remove favorite"):qsTr("Add to favorites");visible:itemData.type==="track";onClicked:music.favorite(itemData)}
                MenuItem {text:backend.cache.pinnedGroups.indexOf(itemData.type+":"+itemData.ratingKey)>=0?qsTr("Unpin download"):qsTr("Pin for offline listening");visible:!itemData.station && (itemData.type==="track" || itemData.type==="album" || itemData.type==="playlist");onClicked:backend.command("pin",{key:itemData.ratingKey,kind:itemData.type,enabled:backend.cache.pinnedGroups.indexOf(itemData.type+":"+itemData.ratingKey)<0})}
                MenuItem {text:qsTr("Move up"); visible:music.showQueue; enabled:index>0; onClicked:music.moveQueue(index,index-1)}
                MenuItem {text:qsTr("Move down"); visible:music.showQueue; enabled:index+1<backend.state.queue.items.length; onClicked:music.moveQueue(index,index+1)}
                MenuItem {text:qsTr("Remove from queue"); visible:music.showQueue; onClicked:backend.command("queue_remove",{index:index})}
            }
            onClicked: music.activate(itemData, index)
            Image {
                id: art; x: Theme.horizontalPageMargin; anchors.verticalCenter: parent.verticalCenter
                height: Math.max(0, Math.min(Theme.iconSizeLarge, parent.height - 2 * Theme.paddingSmall)); width: height
                source: itemData.artwork || ""; asynchronous: true; fillMode: Image.PreserveAspectCrop
            }
            Column {
                id: rowDetails
                anchors.left: art.right; anchors.leftMargin: Theme.paddingMedium; anchors.right: radioAction.visible ? radioAction.left : parent.right
                anchors.rightMargin: Theme.paddingMedium; anchors.verticalCenter: parent.verticalCenter
                Label { width: parent.width; text: itemData.title; truncationMode: TruncationMode.Fade }
                Label { width: parent.width; text: (itemData.type === "artist" ? qsTr("Open artist") : itemData.type === "album" ? (itemData.year > 0 ? itemData.year : qsTr("Open album")) : itemData.type === "playlist" ? music.playlistSummary(itemData) : (itemData.index > 0 ? qsTr("Track") + " " + itemData.index + " · " : "") + music.artist(itemData)) + (itemData.type === "track" && backend.cache.readyKeys.indexOf(itemData.ratingKey) >= 0 ? " · " + qsTr("Downloaded") : ""); color: Theme.secondaryColor; font.pixelSize: Theme.fontSizeExtraSmall; truncationMode: TruncationMode.Fade }
            }
            RadioAction {
                id: radioAction
                anchors.right: parent.right; anchors.rightMargin: Theme.horizontalPageMargin
                anchors.verticalCenter: parent.verticalCenter
                actionName: music.radioActionText(itemData); visible: !pageView.artistBrowse && music.canRadio(itemData); enabled: !backend.busy
                onClicked: music.startRadio(itemData)
            }
        }
        footer: Item {
            width:list.width; height:!pageView.showQueue && pageView.hasMore ? Theme.itemSizeSmall : 0
            BusyIndicator { anchors.centerIn:parent; running:pageView.loadingMore; opacity:running ? 1 : 0; size:BusyIndicatorSize.Small }
            Button { anchors.centerIn:parent; text:qsTr("Retry loading"); visible:pageView.failedPageStart>=0; enabled:!backend.busy && !backend.loadingMore; onClicked:music.retryMore() }
        }
        ViewPlaceholder { enabled: list.count === 0 && !pageView.busy; text: backend.state.serverUrl ? qsTr("No music found") : qsTr("Connect to Plex"); hintText: qsTr("Use the pull-down menu for connection settings") }
        VerticalScrollDecorator {}
    }
    AlphabetRail {
        id:alphabet; groups:pageView.alphabet
        visible:pageKind==="root" && pageView.artistBrowse && groups.length>0
        anchors.right:parent.right; anchors.top:list.top; anchors.bottom:player.top; anchors.topMargin:Theme.paddingLarge; anchors.bottomMargin:Theme.paddingMedium
        width:Theme.itemSizeExtraSmall; fontSize:Theme.fontSizeExtraSmall; color:Theme.secondaryColor; highlightColor:Theme.highlightColor
        onChosen:music.jump(letter)
    }
    DockedPanel {
        id: player; dock: Dock.Bottom; width: parent.width; height: playerContent.height; open: true
        Column {
            id: playerContent
            width: parent.width
            Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; text: backend.state.track ? backend.state.track.title || qsTr("Nothing playing") : qsTr("Nothing playing"); truncationMode: TruncationMode.Fade; color: Theme.highlightColor }
            Button {anchors.horizontalCenter:parent.horizontalCenter;text:qsTr("Now playing");onClicked:music.openPlayer()}
            Row {
                anchors.horizontalCenter: parent.horizontalCenter; spacing: Theme.paddingLarge
                IconButton { icon.source: "image://theme/icon-m-previous"; enabled: !backend.busy; onClicked: backend.position > 3000 ? backend.seek(0) : backend.command("previous") }
                IconButton { icon.source: backend.playing ? "image://theme/icon-m-pause" : "image://theme/icon-m-play"; onClicked: backend.togglePlayback() }
                IconButton { icon.source: "image://theme/icon-m-next"; enabled: !backend.busy; onClicked: backend.command("next", {automatic:false}) }
                IconButton { icon.source: "image://theme/icon-m-shuffle"; highlighted: !!backend.state.queue.shuffled; enabled: !backend.busy && !backend.state.radio; onClicked: backend.command("shuffle", {enabled:!backend.state.queue.shuffled}) }
                IconButton { icon.source: "image://theme/icon-m-repeat"; highlighted: backend.state.queue.repeat !== "off"; enabled: !backend.busy && !backend.state.radio; onClicked: backend.command("repeat", {mode:backend.state.queue.repeat === "off" ? "all" : backend.state.queue.repeat === "all" ? "one" : "off"}) }
            }
            Slider { id:progress;objectName:"playbackSlider";width: parent.width; minimumValue: 0; maximumValue: Math.max(1, backend.duration); value: backend.position; valueText: music.time(backend.position) + " / " + music.time(backend.duration); onReleased: backend.seek(value)
                Connections {target:backend;onPlaybackChanged:if(!progress.down)progress.value=backend.position}
            }
        }
    }
}
