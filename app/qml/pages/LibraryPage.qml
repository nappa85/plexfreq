import QtQuick 2.6
import Sailfish.Silica 1.0
import "../shared"

Page {
    id: page
    property var music
    property string pageKind: "root"
    property string pageKey: ""
    property bool restoringView: false
    property bool controlsExpanded: false
    property bool libraryView: pageKind === "root" && navigation.libraryView
    property bool searchableView: pageKind === "root" && !pageView.showQueue && !!backend.state.serverUrl && pageView.route && (libraryView || pageView.route.op === "search" || pageView.route.op === "discovery_home" || pageView.route.op === "offline_search" || pageView.route.op === "cached_tracks")
    BrowsePageView { id: pageView; objectName:"browsePageView"; music: page.music; pageKind: page.pageKind; pageKey: page.pageKey }
    Navigation {
        id:navigation;objectName:"navigationDefinitions";music:page.music;view:pageView
        interactive:page.status===PageStatus.Active;rootNavigation:page.pageKind==="root"
        onConnectionRequested:pageStack.push(Qt.resolvedUrl("SettingsPage.qml"),{music:page.music})
        onBackRequested:pageStack.depth>1 ? pageStack.pop() : page.music.back()
    }
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
        if (page.status === PageStatus.Active && !data._discarded && music.route && op === music.route.op && music.matchesPage(pageKind, pageKey)) {
            page.restoringView = false
            pageView.thaw()
        }
        page.restorePage()
    }
        onBusyChanged: if (!backend.busy) page.restorePage()
    }
    Connections {
        target: music
        onResetView: if (page.status===PageStatus.Active) list.positionViewAtBeginning()
        onArtistJumped: if (page.status===PageStatus.Active && pageKind==="root") list.positionArtist(index)
    }
    SilicaListView {
        id: list; objectName:"libraryList"; anchors.fill: parent; anchors.bottomMargin: player.height; clip: true
        model: pageView.gridBrowse ? null : pageView.rows
        property real gridContentY: 0
        property bool restoreGridContentY: false
        property int pendingArtistIndex: -1
        property string activeArtistLetter: ""
        // Keep header and rows in one scrolling plane, avoiding transparent
        // PullBackHeader overlap when reversing direction on Qt5.6 Silica.
        headerPositioning:ListView.InlineHeader
        function checkMore() { if (page.status===PageStatus.Active) music.maybeMore(visibleArea.yPosition+visibleArea.heightRatio) }
        function positionArtist(index) {
            pendingArtistIndex=index
            Qt.callLater(tryPositionArtist)
        }
        function tryPositionArtist() {
            var grid=footerItem ? footerItem.grid : null
            if(pendingArtistIndex<0 || !grid || grid.columns<1 || grid.cellHeight<=0)return
            if(pendingArtistIndex>=grid.count && pageView.hasMore)return
            if(grid.height>height && contentHeight<=height)return
            var target=Math.min(pendingArtistIndex,Math.max(0,grid.count-1))
            var point=grid.mapToItem(contentItem,0,Math.floor(target/grid.columns)*grid.cellHeight)
            contentY=Math.max(originY,Math.min(point.y,Math.max(originY,contentHeight-height)))
            pendingArtistIndex=-1
            updateArtistLetter()
        }
        function updateArtistLetter() {
            var grid=footerItem ? footerItem.grid : null
            if(!pageView.artistBrowse || !pageView.alphabet.length || !grid || grid.count===0){activeArtistLetter="";return}
            var localY=grid.mapFromItem(list,0,alphabet.y).y
            var first=Math.max(0,Math.min(grid.count-1,Math.floor(Math.max(0,localY)/grid.cellHeight)*grid.columns))
            var letter=pageView.alphabet[0].letter
            for(var i=0;i<pageView.alphabet.length;i++)if(pageView.alphabet[i].offset<=first)letter=pageView.alphabet[i].letter;else break
            activeArtistLetter=letter
        }
        function retainGridPosition() {
            if (!restoreGridContentY) return
            Qt.callLater(function() {
                contentY = Math.max(originY, Math.min(gridContentY, Math.max(originY, contentHeight - height)))
                restoreGridContentY = false
            })
        }
        onContentYChanged:{moreTimer.restart();updateArtistLetter()}
        onContentHeightChanged:Qt.callLater(tryPositionArtist)
        onCountChanged:moreTimer.restart()
        onHeightChanged:moreTimer.restart()
        Timer { id:moreTimer; interval:100; onTriggered:list.checkMore() }
        Connections { target:backend; onLoadingMoreChanged:if (!backend.loadingMore) {if(pageView.gridBrowse){list.gridContentY=list.contentY;list.restoreGridContentY=true}moreTimer.restart()} }
        PullDownMenu {
            Repeater {
                model:navigation.pageActions
                MenuItem {
                    property var descriptor:navigation.definition(modelData)
                    objectName:"nav_"+modelData;text:descriptor.text;visible:descriptor.visible;enabled:descriptor.enabled
                    onClicked:navigation.activate(modelData)
                }
            }
            MenuItem {text:navigation.definition("offline").text;enabled:navigation.definition("offline").enabled;onClicked:navigation.activate("offline")}
            MenuItem {text:navigation.definition("back").text;visible:navigation.definition("back").visible;enabled:navigation.definition("back").enabled;onClicked:navigation.activate("back")}
        }
        PushUpMenu {
            Repeater {
                model:navigation.secondaryDestinations
                MenuItem {
                    property var descriptor:navigation.definition(modelData)
                    objectName:"nav_"+modelData;text:descriptor.text;visible:descriptor.visible;enabled:descriptor.enabled
                    onClicked:navigation.activate(modelData)
                }
            }
        }
        header: Column {
            objectName:"libraryHeader"
            width: list.width
            height: implicitHeight
            PageHeader { title: pageView.showQueue ? qsTr("Up next") : pageKind === "root" ? "PlexFreq" : pageView.heading; description: pageKind === "root" && !pageView.showQueue ? pageView.heading : "" }
            Row {
                width:parent.width;visible:navigation.rootNavigation
                Repeater {
                    model:navigation.primaryDestinations
                    BackgroundItem {
                        id:destinationControl
                        property var descriptor:navigation.definition(modelData)
                        objectName:descriptor.objectName;width:parent.width/navigation.primaryDestinations.length
                        enabled:descriptor.enabled;highlighted:descriptor.selected
                        onClicked:navigation.activate(modelData)
                        Label {objectName:"destinationLabel";anchors.centerIn:parent;width:parent.width-2*Theme.paddingSmall;text:destinationControl.descriptor.text;horizontalAlignment:Text.AlignHCenter;wrapMode:Text.Wrap;font.pixelSize:Theme.fontSizeExtraSmall;color:destinationControl.highlighted ? Theme.highlightColor : Theme.primaryColor}
                    }
                }
            }
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
            BackgroundItem {
                objectName:"libraryControlsToggle";width:parent.width;visible:page.searchableView || page.libraryView
                onClicked:page.controlsExpanded=!page.controlsExpanded
                Label {
                    x:Theme.horizontalPageMargin;anchors.verticalCenter:parent.verticalCenter;width:parent.width-2*x-Theme.iconSizeSmall
                    text:(pageView.query || qsTr("Search")) + (page.libraryView ? " · " + (music.browseKind==="artist" ? qsTr("Artists") : music.browseKind==="album" ? qsTr("Albums") : qsTr("Tracks")) + " · " + (music.browseSort==="title" ? qsTr("Title") : music.browseSort==="newest" ? qsTr("Recently added") : qsTr("Year")) : "")
                    font.pixelSize:Theme.fontSizeExtraSmall;truncationMode:TruncationMode.Fade
                }
                Label {anchors.right:parent.right;anchors.rightMargin:Theme.horizontalPageMargin;anchors.verticalCenter:parent.verticalCenter;text:page.controlsExpanded ? "−" : "+";color:Theme.highlightColor}
            }
            IntrinsicLoader {
                objectName:"libraryControlsLoader";width:parent.width;active:page.controlsExpanded && (page.searchableView || page.libraryView)
                sourceComponent: Component { Column {
                width:list.width
            SearchField {
                id:artistSearch
                width: parent.width; visible: page.searchableView
                placeholderText: qsTr("Search artists, albums and tracks")
                text:pageView.query
                onTextChanged:if(!pageView.frozen)music.search(text)
                EnterKey.enabled: !backend.busy
                EnterKey.onClicked: { music.submitSearch(text); focus = false }
            }
            ComboBox {objectName:"browseKindControl";width:parent.width;label:qsTr("Browse");visible:page.libraryView;value:music.browseKind==="artist"?qsTr("Artists"):music.browseKind==="album"?qsTr("Albums"):qsTr("Tracks")
                menu:ContextMenu {Repeater {model:["artist","album","track"];MenuItem {text:modelData==="artist"?qsTr("Artists"):modelData==="album"?qsTr("Albums"):qsTr("Tracks");onClicked:{music.browseKind=modelData;if(modelData==="artist" && music.browseSort==="year")music.browseSort="title";music.query="";music.browse()}}}}
            }
            ComboBox {width:parent.width;label:qsTr("Sort");visible:page.libraryView;value:music.browseSort==="title"?qsTr("Title"):music.browseSort==="newest"?qsTr("Recently added"):qsTr("Year")
                menu:ContextMenu {Repeater {model:music.browseKind==="artist"?["title","newest"]:["title","newest","year"];MenuItem {text:modelData==="title"?qsTr("Title"):modelData==="newest"?qsTr("Recently added"):qsTr("Year");onClicked:{music.browseSort=modelData;music.query="";music.browse()}}}}
            }
            }
            } }
            BusyIndicator { anchors.horizontalCenter: parent.horizontalCenter; running: pageView.busy; visible: running; size: BusyIndicatorSize.Medium }
            IntrinsicLoader {
                width: parent.width; active: !!pageView.detail
                sourceComponent: Component { DetailHeader { width: list.width; music: page.music; view:pageView } }
            }
        }
        section.property:pageView.homeView ? "groupTitle" : pageView.route && pageView.route.op==="artist_albums" ? "albumGroup" : ""
        section.delegate:Item {
            property var groupItem:pageView.homeView ? music.groupHub(section,pageView.items) : null
            width:list.width;height:Math.max(groupTitle.height,groupButton.visible ? groupButton.height : 0)
            SectionHeader {id:groupTitle;x:0;width:Math.max(0,groupButton.visible ? groupButton.x-Theme.paddingSmall : parent.width);anchors.verticalCenter:parent.verticalCenter;text:section;horizontalAlignment:Text.AlignLeft;clip:true}
            TextLink {id:groupButton;anchors.right:parent.right;anchors.rightMargin:Theme.horizontalPageMargin;anchors.verticalCenter:parent.verticalCenter;width:Math.min(implicitWidth,parent.width*0.45);text:navigation.showAllLabel;fontSize:Theme.fontSizeExtraSmall;fontFamily:Theme.fontFamily;color:Theme.highlightColor;pressedColor:Theme.primaryColor;minimumTouchHeight:Theme.itemSizeSmall;visible:!!parent.groupItem;enabled:!backend.busy && page.status===PageStatus.Active;onClicked:music.hub(parent.groupItem)}
        }
        delegate: ListItem {
            property var itemData: entry
            property bool discoveryGridItem:pageView.homeView && !!itemData.discoveryGrid
            property var discoveryCards:discoveryGridItem ? music.discoveryGridItems(pageView.items,index) : []
            objectName:discoveryCards.length ? "discoverySectionHost" : ""
            width: list.width - (alphabet.visible ? alphabet.width : 0)
            contentHeight:discoveryGridItem ? (discoveryCards.length ? discoveryGrid.height+Theme.paddingMedium : 0) : Math.max(Theme.itemSizeMedium, rowDetails.height + 2 * Theme.paddingSmall, radioAction.height + 2 * Theme.paddingSmall)
            clip:discoveryGridItem
            enabled: !discoveryGridItem && !backend.busy && page.status === PageStatus.Active
            menu: ContextMenu {
                MenuItem {text:qsTr("Add sonic waypoint");visible:itemData.type==="track";enabled:music.journeyTracks.length<8;onClicked:music.addJourneyTrack(itemData)}
                MenuItem {text:qsTr("Edit smart filters");visible:itemData.type==="playlist" && !!itemData.smart && !itemData.station;enabled:!backend.state.offlineMode;onClicked:music.editFilters("update",itemData)}
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
            onClicked:if(!discoveryGridItem)music.activate(itemData,index)
            Image {
                id: art; x: Theme.horizontalPageMargin; anchors.verticalCenter: parent.verticalCenter
                height: Math.max(0, Math.min(Theme.iconSizeLarge, parent.height - 2 * Theme.paddingSmall)); width: height
                visible:!discoveryGridItem;source: itemData.artwork || ""; asynchronous: true; fillMode: Image.PreserveAspectCrop
            }
            Column {
                id: rowDetails
                visible:!discoveryGridItem
                anchors.left: art.right; anchors.leftMargin: Theme.paddingMedium; anchors.right: radioAction.visible ? radioAction.left : parent.right
                anchors.rightMargin: Theme.paddingMedium; anchors.verticalCenter: parent.verticalCenter
                Label { width: parent.width; text: itemData.title; truncationMode: TruncationMode.Fade }
                Label { width: parent.width; text: (itemData.type === "artist" ? qsTr("Open artist") : itemData.type === "album" ? (itemData.year > 0 ? itemData.year : qsTr("Open album")) : itemData.type === "playlist" ? music.playlistSummary(itemData) : (itemData.index > 0 ? qsTr("Track") + " " + itemData.index + " · " : "") + music.artist(itemData)) + (itemData.type === "track" && backend.cache.readyKeys.indexOf(itemData.ratingKey) >= 0 ? " · " + qsTr("Downloaded") : ""); color: Theme.secondaryColor; font.pixelSize: Theme.fontSizeExtraSmall; truncationMode: TruncationMode.Fade }
            }
            RadioAction {
                id: radioAction
                anchors.right: parent.right; anchors.rightMargin: Theme.horizontalPageMargin
                anchors.verticalCenter: parent.verticalCenter
                actionName: music.radioActionText(itemData); visible: !discoveryGridItem && !pageView.artistBrowse && music.canRadio(itemData); enabled: !backend.busy
                onClicked: music.startRadio(itemData)
            }
            GridView {
                id:discoveryGrid;objectName:visible ? "discoverySectionGrid" : "";anchors.left:parent.left;anchors.right:parent.right;height:discoveryCards.length ? Math.ceil(discoveryCards.length/columns)*cellHeight : 0;clip:true
                visible:discoveryCards.length>0;interactive:false;model:discoveryCards
                property int columns:Math.max(2,Math.floor(width/(Theme.itemSizeExtraLarge*1.35)))
                cellWidth:width/columns;cellHeight:cellWidth+Theme.itemSizeSmall
                delegate:ListItem {
                    property var card:modelData.entry
                    width:discoveryGrid.cellWidth;height:discoveryGrid.cellHeight;contentHeight:height;enabled:!backend.busy && page.status===PageStatus.Active
                    onClicked:music.activate(card,modelData.sourceIndex)
                    menu:ContextMenu {
                        MenuItem {text:qsTr("Play next");visible:card.type==="track" || card.type==="album";onClicked:music.enqueue(card,true)}
                        MenuItem {text:qsTr("Add to queue");visible:card.type==="track" || card.type==="album";onClicked:music.enqueue(card,false)}
                        MenuItem {text:qsTr("Add to playlist");visible:card.type==="track" || card.type==="album";enabled:!backend.state.offlineMode;onClicked:music.playlistEditor("add",card)}
                        MenuItem {text:qsTr("Sonically similar");visible:music.canRadio(card);onClicked:music.sonicNeighbors(card)}
                        MenuItem {text:card.userRating>=10?qsTr("Remove favorite"):qsTr("Add to favorites");visible:card.type==="track";onClicked:music.favorite(card)}
                    }
                    Column {anchors.fill:parent;anchors.margins:Theme.paddingSmall;spacing:Theme.paddingSmall
                        Image {width:parent.width;height:width;source:card.artwork || "";asynchronous:true;fillMode:Image.PreserveAspectCrop}
                        Label {width:parent.width;text:card.title;maximumLineCount:2;wrapMode:Text.Wrap;elide:Text.ElideRight;color:Theme.highlightColor;font.pixelSize:Theme.fontSizeSmall;horizontalAlignment:Text.AlignHCenter}
                    }
                }
            }
        }
        footer: Item {
            width:list.width
            property alias grid:artistGrid
            property int gridCount:artistGrid.count
            property int pagingHeight:!pageView.showQueue && pageView.hasMore ? Theme.itemSizeSmall : 0
            height:artistGrid.height+pagingHeight
            GridView {
                id:artistGrid;objectName:"routeGrid"
                width:parent.width-(alphabet.visible ? alphabet.width : 0)
                height:visible ? Math.ceil(count/columns)*cellHeight : 0
                visible:pageView.gridBrowse
                model:visible ? pageView.rows : null
                onCountChanged:{list.retainGridPosition();list.updateArtistLetter();Qt.callLater(list.tryPositionArtist)}
                onColumnsChanged:{list.updateArtistLetter();Qt.callLater(list.tryPositionArtist)}
                onHeightChanged:{list.updateArtistLetter();Qt.callLater(list.tryPositionArtist)}
                interactive:false
                property int columns:Math.max(2,Math.floor(width/(Theme.itemSizeExtraLarge*1.35)))
                cellWidth:width/columns
                cellHeight:cellWidth+Theme.itemSizeSmall
                delegate:ListItem {
                    property var itemData:entry
                    width:artistGrid.cellWidth;height:artistGrid.cellHeight;contentHeight:height
                    enabled:!backend.busy && page.status===PageStatus.Active
                    onClicked:music.activate(itemData,index)
                    menu:ContextMenu {
                        MenuItem {text:qsTr("Play next");visible:itemData.type==="track" || itemData.type==="album";onClicked:music.enqueue(itemData,true)}
                        MenuItem {text:qsTr("Add to queue");visible:itemData.type==="track" || itemData.type==="album";onClicked:music.enqueue(itemData,false)}
                        MenuItem {text:qsTr("Add to playlist");visible:itemData.type==="track" || itemData.type==="album";enabled:!backend.state.offlineMode;onClicked:music.playlistEditor("add",itemData)}
                        MenuItem {text:qsTr("Sonically similar");visible:music.canRadio(itemData);onClicked:music.sonicNeighbors(itemData)}
                        MenuItem {text:qsTr("Start sonic adventure here");visible:itemData.type==="track";onClicked:music.adventureStart=itemData}
                        MenuItem {text:qsTr("Download radio · %1 minutes").arg(music.downloadMinutes);visible:music.canRadio(itemData);enabled:!backend.state.offlineMode;onClicked:music.downloadRadio(itemData)}
                        MenuItem {text:qsTr("Add as mix seed");visible:itemData.type==="artist" || itemData.type==="album";onClicked:music.addMixSeed(itemData)}
                        MenuItem {text:itemData.userRating>=10?qsTr("Remove favorite"):qsTr("Add to favorites");visible:itemData.type==="track";onClicked:music.favorite(itemData)}
                    }
                    Column {
                        anchors.fill:parent;anchors.margins:Theme.paddingSmall;spacing:Theme.paddingSmall
                        Image {width:parent.width;height:width;source:itemData.artwork || "";asynchronous:true;fillMode:Image.PreserveAspectCrop}
                        Label {width:parent.width;text:itemData.title;maximumLineCount:2;wrapMode:Text.Wrap;elide:Text.ElideRight;color:Theme.highlightColor;font.pixelSize:Theme.fontSizeSmall;horizontalAlignment:Text.AlignHCenter}
                    }
                }
            }
            Item {
                y:artistGrid.height;width:parent.width;height:parent.pagingHeight
                BusyIndicator { anchors.centerIn:parent; running:pageView.loadingMore; opacity:running ? 1 : 0; size:BusyIndicatorSize.Small }
                Button { anchors.centerIn:parent; text:qsTr("Retry loading"); visible:pageView.failedPageStart>=0; enabled:!backend.busy && !backend.loadingMore; onClicked:music.retryMore() }
            }
        }
        ViewPlaceholder { enabled: (pageView.gridBrowse ? (!list.footerItem || list.footerItem.gridCount===0) : list.count===0) && !pageView.busy; text: backend.state.serverUrl ? qsTr("No music found") : qsTr("Connect to Plex"); hintText: qsTr("Use the pull-up menu for connection settings") }
        VerticalScrollDecorator {}
    AlphabetRail {
        id:alphabet; objectName:"libraryAlphabet"; parent:list; groups:pageView.alphabet
        visible:pageKind==="root" && pageView.artistBrowse && groups.length>0
        anchors.right:parent.right
        y:Math.max(0,list.headerItem ? list.headerItem.y + list.headerItem.height - list.contentY : 0) + Theme.paddingSmall
        height:Math.max(0,list.height-y-Theme.paddingMedium)
        width:Math.max(Theme.itemSizeExtraSmall,minimumTouchWidth); fontSize:Theme.fontSizeExtraSmall; color:Theme.secondaryColor; highlightColor:Theme.highlightColor
        current:list.activeArtistLetter
        onChosen:music.jump(letter)
    }
    }
    DockedPanel {
        id: player; dock: Dock.Bottom; width: parent.width; height: playerContent.height; open: true
        Column {
            id: playerContent
            width: parent.width
            Label { x: Theme.horizontalPageMargin; width: parent.width - 2*x; text: backend.state.track ? backend.state.track.title || qsTr("Nothing playing") : qsTr("Nothing playing"); truncationMode: TruncationMode.Fade; color: Theme.highlightColor }
            Row {
                width:parent.width-2*Theme.horizontalPageMargin;anchors.horizontalCenter:parent.horizontalCenter;spacing:Theme.paddingSmall
                Button {width:(parent.width-parent.spacing)/2;text:qsTr("Now playing");onClicked:music.openPlayer()}
                Button {width:(parent.width-parent.spacing)/2;text:navigation.definition(music.showQueue ? "back" : "queue").text;enabled:navigation.definition(music.showQueue ? "back" : "queue").enabled;onClicked:navigation.activate(music.showQueue ? "back" : "queue")}
            }
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
