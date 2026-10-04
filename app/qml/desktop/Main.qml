import QtQuick 2.6
import QtQuick.Controls 2.15
import QtQuick.Layouts 1.3
import "../shared"

ApplicationWindow {
    id: window
    visible: true
    width: 1080; height: 760
    minimumWidth: 560; minimumHeight: 540
    title: "PlexFreq"
    color: "#11151c"
    palette.window: "#11151c"
    palette.windowText: "#edf0f5"
    palette.base: "#1c2430"
    palette.text: "#edf0f5"
    palette.button: "#253040"
    palette.buttonText: "#edf0f5"
    palette.highlight: "#ebad3d"
    Session { id: session }
    NowPlaying {id:nowPlaying;music:session;anchors.centerIn:parent;width:Math.min(window.width-40,600)}
    PlaylistEditor {id:playlistEditor;music:session;anchors.centerIn:parent;width:Math.min(window.width-40,500)}
    Downloads {id:downloadManager;anchors.centerIn:parent;width:Math.min(window.width-40,620)}
    AudioSettings {id:audioSettings;music:session;anchors.centerIn:parent;width:Math.min(window.width-40,600)}
    Connections {target:session;onOpenAudioSettings:audioSettings.open()}
    Connections {target:session;onEditPlaylistRequested:playlistEditor.open();onOpenDownloads:downloadManager.open()}
    Connections {target:session;onOpenPlayer:nowPlaying.open()}
    Connections { target:session; onResetView:list.positionViewAtBeginning() }

    ColumnLayout {
        anchors.fill: parent; anchors.margins: 24; spacing: 14
        RowLayout {
            Label { text: "PlexFreq"; font.pixelSize: 30; font.bold: true; color: "#ebad3d" }
            Label { text: qsTr("Find your frequency."); Layout.fillWidth: true; color: "#9eaabd" }
            BusyIndicator { running: backend.busy; visible: running; Layout.preferredWidth: 32; Layout.preferredHeight: 32 }
            Button { text: qsTr("Connect"); onClicked: settings.open() }
            Button { text: qsTr("Downloads") + " (" + backend.cache.tracks + ")"; enabled: !backend.busy && backend.cache.tracks > 0; onClicked: session.downloads() }
            Button {text:qsTr("Manage");onClicked:session.openDownloads()}
            Button {text:backend.state.offlineMode?qsTr("Go online"):qsTr("Offline library");enabled:!backend.busy;onClicked:backend.command("offline_mode",{enabled:!backend.state.offlineMode})}
        }
        Label { text: backend.error; visible: text.length > 0; color: "#ff998b"; wrapMode: Text.Wrap; Layout.fillWidth: true }
        Label {text:qsTr("Offline · showing saved library metadata");visible:!!backend.state.offline;Layout.fillWidth:true;color:"#9eaabd"}
        Label { text: backend.cache.downloading ? qsTr("Caching audio…") : qsTr("%1 tracks ready offline").arg(backend.cache.tracks); visible: backend.cache.downloading || backend.cache.tracks > 0; color:"#9eaabd"; Layout.fillWidth:true }
        RowLayout {
            visible: !!backend.state.radio
            Label { text: backend.state.radio ? qsTr("Radio") + " · " + backend.state.radio.title : ""; color: "#ebad3d"; Layout.fillWidth: true; wrapMode: Text.Wrap }
            Button { text: qsTr("Stop radio"); enabled: !backend.busy; onClicked: backend.command("stop_radio") }
        }
        RowLayout {
            visible: !!backend.state.serverUrl && !session.detail && !session.showQueue
            ComboBox {
                model: backend.state.libraries; textRole: "title"; enabled: !backend.busy
                onActivated: { session.section = model[currentIndex].key; session.browse() }
            }
            TextField { id: search; placeholderText: qsTr("Search artists, albums and tracks"); Layout.fillWidth: true; text:session.query; onTextChanged:session.search(text); onAccepted:session.submitSearch(text) }
            Button { text: qsTr("Search"); enabled: !backend.busy; onClicked:session.submitSearch(search.text) }
            Button { text: qsTr("Playlists"); enabled: !backend.busy; onClicked: session.load("playlists", {start:0}, qsTr("Playlists"), true) }
            Button {text:qsTr("Discover");enabled:!backend.busy;onClicked:discoverMenu.open()
                Menu {id:discoverMenu
                    MenuItem {text:qsTr("Favorites · 5 stars");onTriggered:session.discovery("favorites")}
                    MenuItem {text:qsTr("Recently added");onTriggered:session.discovery("added")}
                    MenuItem {text:qsTr("Recently played");onTriggered:session.discovery("played")}
                }
            }
        }
        RowLayout {
            Button { text: "‹ " + qsTr("Back"); visible: session.canGoBack; enabled: !backend.busy; onClicked: session.back() }
            Button { text: qsTr("Artists"); visible: session.canGoBack; enabled: !backend.busy; onClicked: session.browse() }
            Label { text: session.showQueue ? qsTr("Up next") : session.heading; font.pixelSize: 23; Layout.fillWidth: true }
            Button { text: session.showQueue ? qsTr("Library") : qsTr("Queue"); onClicked: session.showQueue = !session.showQueue }
            Button {text:qsTr("Saved albums");visible:!!backend.state.offlineMode;enabled:!backend.busy;onClicked:session.load("offline_browse",{kind:"album"},qsTr("Saved albums"),true)}
            Button { text: qsTr("Download tracks"); visible: !session.detail && session.items.length > 0 && session.items[0].type === "track"; enabled: !backend.busy && backend.cache.enabled; onClicked: session.downloadTracks() }
        }
        Label {Layout.fillWidth:true;visible:!!session.playlist;text:session.playlist ? session.playlistSummary(session.playlist) : "";wrapMode:Text.Wrap;color:"#9eaabd"}
        Flow {
            Layout.fillWidth:true;spacing:12
            visible:!session.showQueue && !session.playlist
            ComboBox {model:[qsTr("Artists"),qsTr("Albums"),qsTr("Tracks")];enabled:!backend.busy;onActivated:{session.browseKind=["artist","album","track"][currentIndex];if(session.browseKind==="artist" && session.browseSort==="year")session.browseSort="title";session.query="";session.browse()}}
            ComboBox {model:session.browseKind==="artist"?[qsTr("Title"),qsTr("Recently added")]:[qsTr("Title"),qsTr("Recently added"),qsTr("Year")];enabled:!backend.busy;onActivated:{session.browseSort=["title","newest","year"][currentIndex];session.query="";session.browse()}}
            Button {text:qsTr("Group album types");visible:session.detail && session.detail.type==="artist";enabled:!backend.busy;onClicked:session.groupAlbums()}
            Button {text:qsTr("New playlist");enabled:!backend.busy && !backend.state.offlineMode;onClicked:session.playlistEditor("create",null)}
            Button {text:qsTr("Mix")+" ("+session.mixSeeds.length+")";enabled:!backend.busy && session.mixSeeds.length>0;onClicked:session.playMix();onPressAndHold:session.mixSeeds=[]}
        }
        RowLayout {
        Layout.fillWidth:true; Layout.fillHeight:true
        ListView {
            id: list; Layout.fillWidth: true; Layout.fillHeight: true; clip: true; spacing: 5
            model: session.rows
            function checkMore() { session.maybeMore(visibleArea.yPosition+visibleArea.heightRatio) }
            onContentYChanged:moreTimer.restart()
            onCountChanged:moreTimer.restart()
            onHeightChanged:moreTimer.restart()
            Timer { id:moreTimer; interval:100; onTriggered:list.checkMore() }
            Connections { target:backend; onLoadingMoreChanged:if (!backend.loadingMore) moreTimer.restart() }
            ScrollBar.vertical: ScrollBar {}
            section.property:session.route && session.route.op==="artist_albums" ? "albumGroup" : ""
            section.delegate:Label {width:list.width;text:section;font.pixelSize:21;color:"#ebad3d";padding:8}
            header: IntrinsicLoader {
                width: list.width; active: !!session.detail
                sourceComponent: Component { DetailHeader { width: list.width; music: session } }
            }
            delegate: ItemDelegate {
                property var itemData:entry
                width: list.width; height: 76; enabled: !backend.busy
                onClicked: session.activate(itemData, index)
                contentItem: RowLayout {
                    Rectangle {
                        Layout.preferredWidth: 62; Layout.preferredHeight: 62; radius: 8; color: "#253040"
                        Label { anchors.centerIn: parent; text: "♪"; color: "#ebad3d"; font.pixelSize: 30 }
                        Image { anchors.fill: parent; source: itemData.artwork || ""; fillMode: Image.PreserveAspectCrop; asynchronous: true }
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        Label { text: itemData.title; font.pixelSize: 18; elide: Text.ElideRight; Layout.fillWidth: true }
                        Label { text: itemData.type === "artist" ? qsTr("Open artist") : itemData.type === "album" ? (itemData.year > 0 ? itemData.year : qsTr("Open album")) : itemData.type === "playlist" ? session.playlistSummary(itemData) : session.artist(itemData); color: "#9eaabd"; elide: Text.ElideRight; Layout.fillWidth: true }
                    }
                    Label { text: itemData.type === "track" ? session.time(itemData.duration) : "›"; color: "#9eaabd" }
                    Label { text:qsTr("Downloaded"); visible:itemData.type === "track" && backend.cache.readyKeys.indexOf(itemData.ratingKey) >= 0; color:"#ebad3d" }
                    RadioAction { actionName: session.radioActionText(itemData); visible: !session.artistBrowse && session.canRadio(itemData); enabled: !backend.busy; onClicked: session.startRadio(itemData) }
                    Button {text:"⋮";onClicked:entryMenu.open()
                        Menu {id:entryMenu
                            MenuItem {text:qsTr("Play next");visible:!session.showQueue;onTriggered:session.enqueue(itemData,true)}
                            MenuItem {text:qsTr("Add to queue");visible:!session.showQueue;onTriggered:session.enqueue(itemData,false)}
                            MenuItem {text:qsTr("Add to playlist");visible:itemData.type==="track" || itemData.type==="album";onTriggered:session.playlistEditor("add",itemData)}
                            MenuItem {text:qsTr("Create playlist from this");visible:itemData.type==="track" || itemData.type==="album";onTriggered:session.playlistEditor("create",itemData)}
                            MenuItem {text:qsTr("Add as mix seed");visible:itemData.type==="artist" || itemData.type==="album";onTriggered:session.addMixSeed(itemData)}
                            MenuItem {text:qsTr("Rename playlist");visible:itemData.type==="playlist";onTriggered:session.playlistEditor("rename",itemData)}
                            MenuItem {text:qsTr("Delete playlist");visible:itemData.type==="playlist";onTriggered:backend.command("playlist_edit",{action:"delete",key:itemData.ratingKey})}
                            MenuItem {text:qsTr("Move playlist entry up");visible:!!session.playlist && !session.playlist.smart;enabled:index>0;onTriggered:session.playlistMove(index,false)}
                            MenuItem {text:qsTr("Move playlist entry down");visible:!!session.playlist && !session.playlist.smart;enabled:index+1<session.items.length;onTriggered:session.playlistMove(index,true)}
                            MenuItem {text:qsTr("Remove from playlist");visible:!!session.playlist && !session.playlist.smart;onTriggered:backend.command("playlist_edit",{action:"remove",key:session.playlist.ratingKey,item_id:itemData.playlistItemId})}
                            MenuItem {text:itemData.userRating>=10?qsTr("Remove favorite"):qsTr("Add to favorites");visible:itemData.type==="track";onTriggered:session.favorite(itemData)}
                            MenuItem {text:backend.cache.pinnedGroups.indexOf(itemData.type+":"+itemData.ratingKey)>=0?qsTr("Unpin download"):qsTr("Pin for offline listening");onTriggered:backend.command("pin",{key:itemData.ratingKey,kind:itemData.type,enabled:backend.cache.pinnedGroups.indexOf(itemData.type+":"+itemData.ratingKey)<0})}
                            MenuItem {text:qsTr("Move up");visible:session.showQueue;enabled:index>0;onTriggered:session.moveQueue(index,index-1)}
                            MenuItem {text:qsTr("Move down");visible:session.showQueue;enabled:index+1<backend.state.queue.items.length;onTriggered:session.moveQueue(index,index+1)}
                            MenuItem {text:qsTr("Remove from queue");visible:session.showQueue;onTriggered:backend.command("queue_remove",{index:index})}
                        }
                    }
                }
            }
            footer: Item {
                width:list.width; height:!session.showQueue && backend.state.hasMore ? 44 : 0
                BusyIndicator { anchors.centerIn:parent; running:backend.loadingMore; opacity:running ? 1 : 0; width:36; height:36 }
                Button { anchors.centerIn:parent; text:qsTr("Retry loading"); visible:session.failedPageStart>=0; enabled:!backend.busy && !backend.loadingMore; onClicked:session.retryMore() }
            }
            Label {
                anchors.centerIn: parent; visible: list.count === 0 && !backend.busy
                text: backend.state.serverUrl ? qsTr("No music found.") : qsTr("Connect to your Plex music server to start listening.")
                color: "#9eaabd"; width: parent.width; horizontalAlignment: Text.AlignHCenter; wrapMode: Text.Wrap
            }
        }
        AlphabetRail { groups:session.alphabet; visible:session.artistBrowse && groups.length>0; Layout.preferredWidth:32; Layout.fillHeight:true; color:"#9eaabd"; highlightColor:"#ebad3d"; fontSize:16; onChosen:session.jump(letter) }
        }
        Rectangle { Layout.fillWidth: true; height: 1; color: "#303c4c" }
        RowLayout {
            Button {text:qsTr("Now playing");onClicked:session.openPlayer()}
            ColumnLayout {
                Layout.fillWidth: true
                Label { text: backend.state.track ? backend.state.track.title || qsTr("Nothing playing") : qsTr("Nothing playing"); font.pixelSize: 19 }
                Label { text: backend.state.track ? session.artist(backend.state.track) : ""; color: "#9eaabd" }
            }
            Button { text: "⏮"; enabled: !backend.busy; onClicked: backend.position > 3000 ? backend.seek(0) : backend.command("previous") }
            Button { text: backend.playing ? "⏸" : "▶"; onClicked: backend.togglePlayback() }
            Button { text: "⏭"; enabled: !backend.busy; onClicked: backend.command("next", {automatic:false}) }
            Button { text: qsTr("Shuffle"); checkable: true; checked: !!backend.state.queue.shuffled; enabled: !backend.busy && !backend.state.radio; onClicked: backend.command("shuffle", {enabled: !backend.state.queue.shuffled}) }
            Button { text: qsTr("Repeat") + ": " + backend.state.queue.repeat; enabled: !backend.busy && !backend.state.radio; onClicked: backend.command("repeat", {mode: backend.state.queue.repeat === "off" ? "all" : backend.state.queue.repeat === "all" ? "one" : "off"}) }
        }
        RowLayout {
            Label { text: session.time(backend.position) }
            Slider {id:progress;objectName:"playbackSlider";Layout.fillWidth: true; from: 0; to: Math.max(1, backend.duration); value: backend.position; onMoved: backend.seek(value)
                Connections {target:backend;onPlaybackChanged:if(!progress.pressed)progress.value=backend.position}
            }
            Label { text: session.time(backend.duration) }
            Slider { Layout.preferredWidth: 100; from: 0; to: 1; value: 0.8; onMoved: backend.setVolume(value) }
        }
    }
    Dialog {
        id: settings; title: qsTr("Connect to Plex"); modal: true
        anchors.centerIn: parent; width: Math.min(window.width - 40, 500)
        standardButtons: Dialog.Close
        contentItem: ScrollView {
            id: connectionScroll
            implicitHeight: Math.min(window.height - 140, connectionColumn.implicitHeight)
            contentWidth: availableWidth; clip: true
            ColumnLayout {
            id: connectionColumn; width: connectionScroll.availableWidth
            spacing: 12
            Button { text: qsTr("Sign in with Plex in browser"); enabled: !backend.busy; onClicked: backend.command("login") }
            Label { text: qsTr("Waiting for browser authorization…"); visible: session.polling; wrapMode: Text.Wrap; Layout.fillWidth: true }
            Button { text: qsTr("Open sign-in again"); visible: session.polling; onClicked: Qt.openUrlExternally(session.loginUrl) }
            Label {
                visible: !!backend.state.signedIn || backend.state.servers.length > 0
                text: qsTr("Choose a Plex server"); font.pixelSize: 21; font.bold: true
                color: "#ebad3d"; Layout.fillWidth: true; wrapMode: Text.Wrap
            }
            Label {
                visible: !!backend.state.signedIn || backend.state.servers.length > 0
                text: backend.state.servers.length > 0 ? qsTr("Select a server to open its music libraries.") : qsTr("Refresh the list to find your Plex servers.")
                color: "#9eaabd"; Layout.fillWidth: true; wrapMode: Text.Wrap
            }
            Button { text: qsTr("Refresh servers"); visible: !!backend.state.signedIn; enabled: !backend.busy; onClicked: backend.command("servers") }
            Frame {
                visible: backend.state.servers.length > 0
                Layout.fillWidth: true; Layout.preferredHeight: Math.min(240, serverList.contentHeight) + 2 * padding
                background: Rectangle { color: "#1c2430"; border.color: "#ebad3d"; radius: 8 }
                ListView {
                    id: serverList; anchors.fill: parent; clip: true; spacing: 6
                    model: backend.state.servers
                    ScrollBar.vertical: ScrollBar {}
                    delegate: ItemDelegate {
                        width: serverList.width; height: Math.max(76, serverInfo.implicitHeight + 24)
                        enabled: !backend.busy
                        onClicked: { backend.command("select_server", {index:index}); settings.close() }
                        contentItem: RowLayout {
                            Rectangle {
                                Layout.preferredWidth: 40; Layout.preferredHeight: 40
                                radius: 8; color: "#ebad3d"
                                Label { anchors.centerIn: parent; text: "P"; color: "#11151c"; font.pixelSize: 23; font.bold: true }
                            }
                            ColumnLayout {
                                id: serverInfo; Layout.fillWidth: true
                                Label { text: modelData.name; font.pixelSize: 19; font.bold: true; color: "#ebad3d"; Layout.fillWidth: true; wrapMode: Text.Wrap }
                                Label { text: qsTr("Connect to music libraries"); color: "#9eaabd"; Layout.fillWidth: true; wrapMode: Text.Wrap }
                            }
                            Label { text: "›"; color: "#ebad3d"; font.pixelSize: 26 }
                        }
                    }
                }
            }
            Label { text: qsTr("Direct connection"); font.bold: true; Layout.fillWidth: true }
            Label { text: qsTr("Or connect directly using a server URL and Plex token:"); wrapMode: Text.Wrap; Layout.fillWidth: true }
            TextField { id: address; Layout.fillWidth: true; placeholderText: "http://192.168.1.10:32400"; text: backend.state.serverUrl || "" }
            TextField { id: token; Layout.fillWidth: true; placeholderText: qsTr("X-Plex-Token"); echoMode: TextInput.Password }
            Button { text: qsTr("Connect directly"); enabled: !backend.busy && address.text.length > 0; onClicked: { backend.command("connect", {url:address.text, token:token.text}); token.clear(); settings.close() } }
            Button { text: qsTr("Sign out and clear saved tokens"); enabled: !backend.busy; onClicked: { backend.command("logout"); token.clear() } }
            Label { text:qsTr("Offline audio cache"); font.bold:true; Layout.fillWidth:true }
            Button {text:qsTr("Audio settings");onClicked:session.openAudioSettings()}
            Switch {text:qsTr("Autoplay related music at queue end");checked:!!backend.state.autoplay;enabled:!backend.busy;onClicked:backend.command("autoplay",{enabled:checked})}
            Switch { text:qsTr("Cache queued audio automatically"); checked:backend.cache.enabled; enabled:!backend.busy; onClicked:backend.command("cache_config", {enabled:checked, limit_mb:backend.cache.limitMb, ahead:backend.cache.ahead}) }
            ComboBox { model:[128,256,512,1024,2048]; enabled:!backend.busy; displayText:qsTr("Cache limit") + ": " + backend.cache.limitMb + " MiB"; onActivated:backend.command("cache_config", {enabled:backend.cache.enabled, limit_mb:model[currentIndex], ahead:backend.cache.ahead}) }
            Label { text:qsTr("%1 tracks downloaded · %2 MiB used").arg(backend.cache.tracks).arg(Math.round(backend.cache.bytes / 1048576)); Layout.fillWidth:true }
            Label { text:backend.cache.error; visible:text.length>0; color:"#ff998b"; wrapMode:Text.Wrap; Layout.fillWidth:true }
            Button { text:qsTr("Clear cache and stop playback"); enabled:!backend.busy; onClicked:backend.command("clear_cache") }
            }
        }
    }
}
