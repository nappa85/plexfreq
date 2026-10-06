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

    ColumnLayout {
        anchors.fill: parent; anchors.margins: 24; spacing: 14
        RowLayout {
            Label { text: "PlexFreq"; font.pixelSize: 30; font.bold: true; color: "#ebad3d" }
            Label { text: qsTr("Find your frequency."); Layout.fillWidth: true; color: "#9eaabd" }
            BusyIndicator { running: backend.busy; visible: running; Layout.preferredWidth: 32; Layout.preferredHeight: 32 }
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
            property real gridContentY: 0
            property bool restoreGridContentY: false
            property int pendingArtistIndex: -1
            property string activeArtistLetter: ""
            function checkMore() { session.maybeMore(visibleArea.yPosition+visibleArea.heightRatio) }
            function positionArtist(index) {
                pendingArtistIndex=index
                Qt.callLater(tryPositionArtist)
            }
            function tryPositionArtist() {
                var grid=footerItem ? footerItem.grid : null
                if(pendingArtistIndex<0 || !grid || grid.columns<1 || grid.cellHeight<=0)return
                if(pendingArtistIndex>=grid.count && backend.state.hasMore)return
                if(grid.height>height && contentHeight<=height)return
                var target=Math.min(pendingArtistIndex,Math.max(0,grid.count-1))
                var point=grid.mapToItem(contentItem,0,Math.floor(target/grid.columns)*grid.cellHeight)
                contentY=Math.max(originY,Math.min(point.y,Math.max(originY,contentHeight-height)))
                pendingArtistIndex=-1
                updateArtistLetter()
            }
            function updateArtistLetter() {
                var grid=footerItem ? footerItem.grid : null
                if(!session.artistBrowse || !session.alphabet.length || !grid || grid.count===0){activeArtistLetter="";return}
                var localY=grid.mapFromItem(list,0,0).y
                var first=Math.max(0,Math.min(grid.count-1,Math.floor(Math.max(0,localY)/grid.cellHeight)*grid.columns))
                var letter=session.alphabet[0].letter
                for(var i=0;i<session.alphabet.length;i++)if(session.alphabet[i].offset<=first)letter=session.alphabet[i].letter;else break
                activeArtistLetter=letter
            }
            function retainGridPosition() {
                if(!restoreGridContentY)return
                Qt.callLater(function(){contentY=Math.max(originY,Math.min(gridContentY,Math.max(originY,contentHeight-height)));restoreGridContentY=false})
            }
            onContentYChanged:{moreTimer.restart();updateArtistLetter()}
            onContentHeightChanged:Qt.callLater(tryPositionArtist)
            onCountChanged:moreTimer.restart()
            onHeightChanged:moreTimer.restart()
            Timer { id:moreTimer; interval:100; onTriggered:list.checkMore() }
            Connections { target:backend; onLoadingMoreChanged:if (!backend.loadingMore) {if(session.gridBrowse){list.gridContentY=list.contentY;list.restoreGridContentY=true}moreTimer.restart()} }
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
                        Menu {id:entryMenu
                            MenuItem {text:qsTr("Add sonic waypoint");visible:itemData.type==="track";enabled:session.journeyTracks.length<8;onTriggered:session.addJourneyTrack(itemData)}
                            MenuItem {text:qsTr("Edit smart filters");visible:itemData.type==="playlist" && !!itemData.smart && !itemData.station;enabled:!backend.state.offlineMode;onTriggered:session.editFilters("update",itemData)}
                            MenuItem {text:qsTr("Sonically similar");visible:session.canRadio(itemData);onTriggered:session.sonicNeighbors(itemData)}
                            MenuItem {text:qsTr("Start sonic adventure here");visible:itemData.type==="track";onTriggered:session.adventureStart=itemData}
                            MenuItem {text:qsTr("Sonic Adventure to this track");visible:itemData.type==="track" && !!session.adventureStart;onTriggered:session.sonicAdventure(itemData)}
                            MenuItem {text:qsTr("Download radio · %1 minutes").arg(session.downloadMinutes);visible:itemData.station || session.canRadio(itemData);enabled:!backend.state.offlineMode;onTriggered:session.downloadRadio(itemData)}
                            MenuItem {text:qsTr("Download %1 minutes").arg(session.downloadMinutes);visible:itemData.type==="playlist" && !itemData.station;enabled:!backend.state.offlineMode;onTriggered:backend.command("download_plan",{kind:"playlist",key:itemData.ratingKey,minutes:session.downloadMinutes})}
                            MenuItem {text:qsTr("Play next");visible:!session.showQueue;onTriggered:session.enqueue(itemData,true)}
                            MenuItem {text:qsTr("Add to queue");visible:!session.showQueue;onTriggered:session.enqueue(itemData,false)}
                            MenuItem {text:qsTr("Add to playlist");visible:itemData.type==="track" || itemData.type==="album";onTriggered:session.playlistEditor("add",itemData)}
                            MenuItem {text:qsTr("Create playlist from this");visible:itemData.type==="track" || itemData.type==="album";onTriggered:session.playlistEditor("create",itemData)}
                            MenuItem {text:qsTr("Add as mix seed");visible:itemData.type==="artist" || itemData.type==="album";onTriggered:session.addMixSeed(itemData)}
                            MenuItem {text:qsTr("Rename playlist");visible:itemData.type==="playlist" && !itemData.station;onTriggered:session.playlistEditor("rename",itemData)}
                            MenuItem {text:qsTr("Delete playlist");visible:itemData.type==="playlist" && !itemData.station;onTriggered:backend.command("playlist_edit",{action:"delete",key:itemData.ratingKey})}
                            MenuItem {text:qsTr("Move playlist entry up");visible:!!session.playlist && !session.playlist.smart;enabled:index>0;onTriggered:session.playlistMove(index,false)}
                            MenuItem {text:qsTr("Move playlist entry down");visible:!!session.playlist && !session.playlist.smart;enabled:index+1<session.items.length;onTriggered:session.playlistMove(index,true)}
                            MenuItem {text:qsTr("Remove from playlist");visible:!!session.playlist && !session.playlist.smart;onTriggered:backend.command("playlist_edit",{action:"remove",key:session.playlist.ratingKey,item_id:itemData.playlistItemId})}
                            MenuItem {text:itemData.userRating>=10?qsTr("Remove favorite"):qsTr("Add to favorites");visible:itemData.type==="track";onTriggered:session.favorite(itemData)}
                            MenuItem {text:backend.cache.pinnedGroups.indexOf(itemData.type+":"+itemData.ratingKey)>=0?qsTr("Unpin download"):qsTr("Pin for offline listening");visible:!itemData.station && (itemData.type==="track" || itemData.type==="album" || itemData.type==="playlist");onTriggered:backend.command("pin",{key:itemData.ratingKey,kind:itemData.type,enabled:backend.cache.pinnedGroups.indexOf(itemData.type+":"+itemData.ratingKey)<0})}
                            MenuItem {text:qsTr("Move up");visible:session.showQueue;enabled:index>0;onTriggered:session.moveQueue(index,index-1)}
                            MenuItem {text:qsTr("Move down");visible:session.showQueue;enabled:index+1<backend.state.queue.items.length;onTriggered:session.moveQueue(index,index+1)}
                            MenuItem {text:qsTr("Remove from queue");visible:session.showQueue;onTriggered:backend.command("queue_remove",{index:index})}
                        }
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
                            Menu {id:cardMenu
                                MenuItem {text:qsTr("Play next");visible:card.type==="track" || card.type==="album";onTriggered:session.enqueue(card,true)}
                                MenuItem {text:qsTr("Add to queue");visible:card.type==="track" || card.type==="album";onTriggered:session.enqueue(card,false)}
                                MenuItem {text:qsTr("Add to playlist");visible:card.type==="track" || card.type==="album";enabled:!backend.state.offlineMode;onTriggered:session.playlistEditor("add",card)}
                                MenuItem {text:qsTr("Sonically similar");visible:session.canRadio(card);onTriggered:session.sonicNeighbors(card)}
                                MenuItem {text:card.userRating>=10?qsTr("Remove favorite"):qsTr("Add to favorites");visible:card.type==="track";onTriggered:session.favorite(card)}
                            }
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
                    onCountChanged:{list.retainGridPosition();list.updateArtistLetter();Qt.callLater(list.tryPositionArtist)}
                    onColumnsChanged:{list.updateArtistLetter();Qt.callLater(list.tryPositionArtist)}
                    onHeightChanged:{list.updateArtistLetter();Qt.callLater(list.tryPositionArtist)}
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
                            Menu {id:artistMenu
                                MenuItem {text:qsTr("Play next");visible:itemData.type==="track" || itemData.type==="album";onTriggered:session.enqueue(itemData,true)}
                                MenuItem {text:qsTr("Add to queue");visible:itemData.type==="track" || itemData.type==="album";onTriggered:session.enqueue(itemData,false)}
                                MenuItem {text:qsTr("Add to playlist");visible:itemData.type==="track" || itemData.type==="album";enabled:!backend.state.offlineMode;onTriggered:session.playlistEditor("add",itemData)}
                                MenuItem {text:qsTr("Sonically similar");visible:session.canRadio(itemData);onTriggered:session.sonicNeighbors(itemData)}
                                MenuItem {text:qsTr("Start sonic adventure here");visible:itemData.type==="track";onTriggered:session.adventureStart=itemData}
                                MenuItem {text:qsTr("Download radio · %1 minutes").arg(session.downloadMinutes);visible:session.canRadio(itemData);enabled:!backend.state.offlineMode;onTriggered:session.downloadRadio(itemData)}
                                MenuItem {text:qsTr("Add as mix seed");visible:itemData.type==="artist" || itemData.type==="album";onTriggered:session.addMixSeed(itemData)}
                                MenuItem {text:itemData.userRating>=10?qsTr("Remove favorite"):qsTr("Add to favorites");visible:itemData.type==="track";onTriggered:session.favorite(itemData)}
                            }
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
        Connections {target:session;onArtistJumped:list.positionArtist(index)}
        AlphabetRail {objectName:"libraryAlphabet";groups:session.alphabet;current:list.activeArtistLetter;visible:session.artistBrowse && groups.length>0;Layout.preferredWidth:Math.max(44,implicitWidth);Layout.fillHeight:true;color:"#9eaabd";highlightColor:"#ebad3d";fontSize:16;onChosen:session.jump(letter)}
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
