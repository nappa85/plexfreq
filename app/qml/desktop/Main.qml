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
    property bool libraryView: navigation.libraryView
    property alias navigationCatalogue: navigation
    property bool showBack: navigation.definition("back").visible
    Session { id: session; objectName:"musicSession" }
    Navigation {
        id:navigation;objectName:"navigationDefinitions";music:session
        onConnectionRequested:settings.open()
        onBackRequested:session.back()
    }
    NowPlaying {id:nowPlaying;music:session;anchors.centerIn:parent;width:Math.min(window.width-40,600)}
    PlaylistEditor {id:playlistEditor;music:session;anchors.centerIn:parent;width:Math.min(window.width-40,500)}
    Downloads {id:downloadManager;music:session;anchors.centerIn:parent;width:Math.min(window.width-40,620)}
    AudioSettings {id:audioSettings;music:session;anchors.centerIn:parent;width:Math.min(window.width-40,600)}
    Filters {id:filterEditor;music:session;anchors.centerIn:parent;width:Math.min(window.width-40,500)}
    DiscoverySettings {id:discoverySettings;music:session;anchors.centerIn:parent;width:Math.min(window.width-40,500)}
    Insights {id:insights;music:session;anchors.centerIn:parent;width:Math.min(window.width-40,600)}
    Connections {target:session;onOpenFilterEditor:filterEditor.open();onOpenDiscoverySettings:discoverySettings.open();onOpenInsights:insights.open()}
    Connections {target:session;onOpenAudioSettings:audioSettings.open()}
    Connections {target:session;onEditPlaylistRequested:playlistEditor.open();onOpenDownloads:downloadManager.open()}
    Connections {target:session;onOpenPlayer:nowPlaying.open()}
    Connections { target:session; onResetView:list.positionViewAtBeginning() }
    ItemActions {id:itemActions;music:session}
    GridPosition {id:gridPosition;listView:list;artistBrowse:session.artistBrowse;alphabet:session.alphabet;hasMore:!!backend.state.hasMore}

    ColumnLayout {
        anchors.fill: parent; anchors.margins: 24; spacing: 14
        RowLayout {
            Label { text: "PlexFreq"; font.pixelSize: 30; font.bold: true; color: "#ebad3d" }
            Label { text: qsTr("Find your frequency."); Layout.fillWidth: true; color: "#9eaabd" }
            BusyIndicator { objectName:"foregroundLoadingIndicator";running: backend.busy || backend.buffering; visible: running; Layout.preferredWidth: 32; Layout.preferredHeight: 32 }
            Switch {text:navigation.offlineLabel;checked:navigation.offlineEnabled;enabled:navigation.ready;onClicked:navigation.setOffline(checked)}
        }
        Label { text: backend.error; visible: text.length > 0; color: "#ff998b"; wrapMode: Text.Wrap; Layout.fillWidth: true }
        Label {text:qsTr("Offline · showing saved library metadata");visible:!!backend.state.offline;Layout.fillWidth:true;color:"#9eaabd"}
        Label { text: backend.cache.downloading ? qsTr("Caching audio…") : qsTr("%1 tracks ready offline").arg(backend.cache.tracks); visible: backend.cache.downloading || backend.cache.tracks > 0; color:"#9eaabd"; Layout.fillWidth:true }
        RowLayout {
            visible: !!backend.state.radio
            Label { text: backend.state.radio ? qsTr("Radio") + " · " + backend.state.radio.title : ""; color: "#ebad3d"; Layout.fillWidth: true; wrapMode: Text.Wrap }
        }
        RowLayout {
            Layout.fillWidth:true;spacing:8
            ToolButton {
                id:destinationsButton
                text:"☰";font.pixelSize:24;Layout.preferredWidth:44;Layout.preferredHeight:44
                onClicked:destinationsMenu.open()
                Accessible.name:navigation.destinationsTitle
                ToolTip.text:navigation.destinationsTitle;ToolTip.visible:hovered;ToolTip.delay:500
            }
            Repeater {
                model:navigation.primaryDestinations
                Button {
                    property var descriptor:navigation.definition(modelData)
                    objectName:descriptor.objectName;text:descriptor.text
                    Layout.fillWidth:true;Layout.preferredWidth:1;Layout.minimumWidth:0;Layout.preferredHeight:44
                    highlighted:descriptor.selected;enabled:descriptor.enabled
                    onClicked:navigation.activate(modelData)
                }
            }
            ToolButton {text:"⋮";font.pixelSize:24;Layout.preferredWidth:44;Layout.preferredHeight:44;enabled:!backend.busy;onClicked:actionsMenu.open()
                Accessible.name:navigation.actionsTitle
                ToolTip.text:navigation.actionsTitle;ToolTip.visible:hovered;ToolTip.delay:500
                NavigationMenu {id:actionsMenu;objectName:"pageActionsMenu";navigation:window.navigationCatalogue;actionIds:navigation.pageActions;y:parent.height}
            }
            NavigationMenu {id:destinationsMenu;objectName:"destinationsMenu";parent:destinationsButton;navigation:window.navigationCatalogue;actionIds:navigation.secondaryDestinations;y:parent.height}
        }
        RowLayout {
            visible:!!backend.state.serverUrl && !session.detail && !session.showQueue
            ComboBox {
                visible:backend.state.libraries.length>1
                model:backend.state.libraries;textRole:"title";enabled:!backend.busy
                onActivated:{session.section=model[currentIndex].key;session.goHome(true)}
            }
            TextField {id:search;placeholderText:qsTr("Search artists, albums and tracks");Layout.fillWidth:true;text:session.query;onTextChanged:session.search(text);onAccepted:session.submitSearch(text)}
            Button {text:qsTr("Search");enabled:!backend.busy;onClicked:session.submitSearch(search.text)}
        }
        RowLayout {
            property bool showHeading:session.showQueue || (!session.detail && !window.libraryView && !session.homeView && (!session.route || session.route.op!=="playlists"))
            visible:window.showBack || showHeading
            Button {text:"‹ " + navigation.definition("back").text;visible:window.showBack;enabled:navigation.definition("back").enabled;onClicked:navigation.activate("back")}
            Label {visible:parent.showHeading;text:session.showQueue ? qsTr("Up next") : session.heading;font.pixelSize:23;Layout.fillWidth:true}
            Item {visible:!parent.showHeading;Layout.fillWidth:true}
        }
        Label {Layout.fillWidth:true;visible:!!session.playlist;text:session.playlist ? session.playlistSummary(session.playlist) : "";wrapMode:Text.Wrap;color:"#9eaabd"}
        Flow {
            Layout.fillWidth:true;spacing:12
            visible:window.libraryView
            ComboBox {visible:window.libraryView;model:[qsTr("Artists"),qsTr("Albums"),qsTr("Tracks")];currentIndex:["artist","album","track"].indexOf(session.browseKind);enabled:!backend.busy;onActivated:{session.browseKind=["artist","album","track"][currentIndex];if(session.browseKind==="artist" && session.browseSort==="year")session.browseSort="title";session.query="";session.browseLibrary(null)}}
            ComboBox {visible:window.libraryView;model:session.browseKind==="artist"?[qsTr("Title"),qsTr("Recently added")]:[qsTr("Title"),qsTr("Recently added"),qsTr("Year")];currentIndex:["title","newest","year"].indexOf(session.browseSort);enabled:!backend.busy;onActivated:{session.browseSort=["title","newest","year"][currentIndex];session.query="";session.browseLibrary(null)}}
        }
        RowLayout {
        Layout.fillWidth:true; Layout.fillHeight:true
        ListView {
            id:list;objectName:"libraryList";Layout.fillWidth:true;Layout.fillHeight:true;clip:true;spacing:5
            model: session.gridBrowse ? null : session.rows
            function checkMore() { session.maybeMore(visibleArea.yPosition+visibleArea.heightRatio) }
            onContentYChanged:{moreTimer.restart();gridPosition.updateArtistLetter()}
            onContentHeightChanged:gridPosition.schedulePosition()
            onCountChanged:moreTimer.restart()
            onHeightChanged:moreTimer.restart()
            Timer { id:moreTimer; interval:100; onTriggered:list.checkMore() }
            Connections { target:backend; onLoadingMoreChanged:if (!backend.loadingMore) {if(session.gridBrowse)gridPosition.rememberGridPosition();moreTimer.restart()} }
            ScrollBar.vertical: ScrollBar {}
            section.property:session.homeView ? "groupTitle" : session.route && session.route.op==="artist_albums" ? "albumGroup" : ""
            section.delegate:RowLayout {
                property var groupItem:session.homeView ? session.groupHub(section,session.items) : null
                width:list.width;height:implicitHeight
                Label {Layout.fillWidth:true;Layout.minimumWidth:0;text:section;font.pixelSize:21;color:"#ebad3d";padding:8;leftPadding:0;wrapMode:Text.Wrap}
                TextLink {text:navigation.showAllLabel;color:window.palette.highlight;pressedColor:window.palette.text;minimumTouchHeight:32;visible:!!parent.groupItem;Layout.maximumWidth:list.width*0.45;enabled:!backend.busy;onClicked:session.hub(parent.groupItem)}
            }
            header: IntrinsicLoader {
                width: list.width; active: !!session.detail
                sourceComponent: Component { DetailHeader { width: list.width; music: session } }
            }
            delegate: ItemDelegate {
                property var itemData:entry
                property bool discoveryGridItem:session.homeView && !!itemData.discoveryGrid
                property var discoveryCards:discoveryGridItem ? session.discoveryGridItems(session.items,index) : []
                objectName:discoveryCards.length ? "discoverySectionHost" : ""
                width:list.width;height:discoveryGridItem ? (discoveryCards.length ? discoveryGrid.height+12 : 0) : 76;enabled:!discoveryGridItem && !backend.busy;clip:discoveryGridItem
                onClicked:if(!discoveryGridItem)session.activate(itemData,index)
                contentItem:Item {
                RowLayout {anchors.fill:parent;visible:!discoveryGridItem
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
                        ItemMenu {id:entryMenu;actions:itemActions;actionIds:itemActions.rowActions;item:itemData;itemIndex:index}
                    }
                }
                GridView {
                    id:discoveryGrid;objectName:visible ? "discoverySectionGrid" : "";anchors.left:parent.left;anchors.right:parent.right;anchors.top:parent.top;height:discoveryCards.length ? Math.ceil(discoveryCards.length/columns)*cellHeight : 0;visible:discoveryCards.length>0;interactive:false;model:discoveryCards;clip:true
                    property int columns:Math.max(1,Math.floor(width/170));cellWidth:width/columns;cellHeight:180
                    delegate:ItemDelegate {
                        property var card:modelData.entry
                        width:discoveryGrid.cellWidth;height:discoveryGrid.cellHeight;enabled:!backend.busy
                        onClicked:session.activate(card,modelData.sourceIndex)
                        contentItem:Column {spacing:8
                            Image {x:8;width:parent.width-16;height:136;source:card.artwork || "";asynchronous:true;fillMode:Image.PreserveAspectCrop}
                            Label {x:8;width:parent.width-16;text:card.title;maximumLineCount:2;wrapMode:Text.Wrap;elide:Text.ElideRight;color:"#ebad3d";horizontalAlignment:Text.AlignHCenter}
                        }
                        Button {anchors.right:parent.right;anchors.top:parent.top;text:"⋮";onClicked:cardMenu.open()
                            ItemMenu {id:cardMenu;actions:itemActions;actionIds:itemActions.cardActions;item:card;itemIndex:modelData.sourceIndex}
                        }
                    }
                }
                }
            }
            footer: Item {
                width:list.width
                property alias grid:artistGrid
                property int gridCount:artistGrid.count
                property int pagingHeight:!session.showQueue && backend.state.hasMore ? 44 : 0
                height:artistGrid.height+pagingHeight
                GridView {
                    id:artistGrid;objectName:"routeGrid";width:parent.width;height:visible ? Math.ceil(count/columns)*cellHeight : 0
                    visible:session.gridBrowse;model:visible ? session.rows : null;interactive:false
                    onCountChanged:{gridPosition.retainGridPosition();gridPosition.relayout()}
                    onColumnsChanged:gridPosition.relayout()
                    onHeightChanged:gridPosition.relayout()
                    property int columns:Math.max(1,Math.floor(width/170))
                    cellWidth:width/columns;cellHeight:180
                    delegate:ItemDelegate {
                        property var itemData:entry
                        width:artistGrid.cellWidth;height:artistGrid.cellHeight;enabled:!backend.busy
                        onClicked:session.activate(itemData,index)
                        contentItem:Column {
                            spacing:8
                            Image {x:8;width:parent.width-16;height:136;source:itemData.artwork || "";asynchronous:true;fillMode:Image.PreserveAspectCrop}
                            Label {x:8;width:parent.width-16;text:itemData.title;maximumLineCount:2;wrapMode:Text.Wrap;elide:Text.ElideRight;color:"#ebad3d";horizontalAlignment:Text.AlignHCenter}
                        }
                        Button {anchors.right:parent.right;anchors.top:parent.top;text:"⋮";onClicked:artistMenu.open()
                            ItemMenu {id:artistMenu;actions:itemActions;actionIds:itemActions.gridActions;item:itemData;itemIndex:index}
                        }
                    }
                }
                Item {
                    y:artistGrid.height;width:parent.width;height:parent.pagingHeight
                    BusyIndicator { anchors.centerIn:parent; running:backend.loadingMore; opacity:running ? 1 : 0; width:36; height:36 }
                    Button { anchors.centerIn:parent; text:qsTr("Retry loading"); visible:session.failedPageStart>=0; enabled:!backend.busy && !backend.loadingMore; onClicked:session.retryMore() }
                }
            }
            Label {
                anchors.centerIn: parent; visible: (session.gridBrowse ? (!list.footerItem || list.footerItem.gridCount===0) : list.count===0) && !backend.busy
                text: backend.state.serverUrl ? qsTr("No music found.") : qsTr("Connect to your Plex music server to start listening.")
                color: "#9eaabd"; width: parent.width; horizontalAlignment: Text.AlignHCenter; wrapMode: Text.Wrap
            }
        }
        Connections {target:session;onArtistJumped:gridPosition.positionArtist(index)}
        AlphabetRail {objectName:"libraryAlphabet";groups:session.alphabet;current:gridPosition.activeArtistLetter;visible:session.artistBrowse && groups.length>0;Layout.preferredWidth:Math.max(44,implicitWidth);Layout.fillHeight:true;color:"#9eaabd";highlightColor:"#ebad3d";fontSize:16;onChosen:session.jump(letter)}
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
            Button { text: backend.state.queue.repeat === "one" ? qsTr("Repeat one") : backend.state.queue.repeat === "all" ? qsTr("Repeat all") : qsTr("Repeat off"); enabled: !backend.busy && !backend.state.radio; onClicked: backend.command("repeat", {mode: backend.state.queue.repeat === "off" ? "all" : backend.state.queue.repeat === "all" ? "one" : "off"}) }
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
            ComboBox { textRole:"text"; valueRole:"value"; model:[128,256,512,1024,2048].map(function(mb){return {value:mb,text:mb + " MiB"}}); currentIndex:count>0 ? indexOfValue(backend.cache.limitMb) : -1; enabled:!backend.busy; displayText:qsTr("Cache limit") + ": " + backend.cache.limitMb + " MiB"; onActivated:backend.command("cache_config", {enabled:backend.cache.enabled, limit_mb:currentValue, ahead:backend.cache.ahead}) }
            Label { text:qsTr("%1 tracks downloaded · %2 MiB used").arg(backend.cache.tracks).arg(Math.round(backend.cache.bytes / 1048576)); Layout.fillWidth:true }
            Label { text:backend.cache.error; visible:text.length>0; color:"#ff998b"; wrapMode:Text.Wrap; Layout.fillWidth:true }
            Button { text:qsTr("Clear cache and stop playback"); enabled:!backend.busy; onClicked:backend.command("clear_cache") }
            }
        }
    }
}
