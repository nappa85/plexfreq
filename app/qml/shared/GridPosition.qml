import QtQuick 2.6

// Shared artist-grid positioning for the Silica and desktop library lists.
// The grid is the list footer's `grid`. Qt 5.6 lacks Qt.callLater, so
// single-shot zero-interval Timers defer work until pending layout changes
// are applied; restarting a pending Timer coalesces repeated requests.
Item {
    id: helper
    visible: false
    property var listView
    property var grid: listView && listView.footerItem ? listView.footerItem.grid : null
    property bool artistBrowse: false
    property var alphabet: []
    property bool hasMore: false
    property int pendingArtistIndex: -1
    property string activeArtistLetter: ""
    property real gridContentY: 0
    property bool restoreGridContentY: false

    function positionArtist(index) {
        pendingArtistIndex=index
        positionLater.restart()
    }
    function schedulePosition() {
        positionLater.restart()
    }
    function tryPositionArtist() {
        if(pendingArtistIndex<0 || !listView || !grid || grid.columns<1 || grid.cellHeight<=0)return
        if(pendingArtistIndex>=grid.count && hasMore)return
        if(grid.height>listView.height && listView.contentHeight<=listView.height)return
        var target=Math.min(pendingArtistIndex,Math.max(0,grid.count-1))
        var point=grid.mapToItem(listView.contentItem,0,Math.floor(target/grid.columns)*grid.cellHeight)
        listView.contentY=Math.max(listView.originY,Math.min(point.y,Math.max(listView.originY,listView.contentHeight-listView.height)))
        pendingArtistIndex=-1
        updateArtistLetter()
    }
    // The active letter is the one of the topmost grid row that is at least
    // half visible at the top of the listView viewport. Inline headers are listView
    // content and need no inset; rounding to the row midpoint keeps a row that
    // was scrolled exactly to the top (jump) selected despite float rounding.
    function updateArtistLetter() {
        if(!artistBrowse || !alphabet.length || !listView || !grid || grid.count===0 || grid.cellHeight<=0){activeArtistLetter="";return}
        var localY=Math.max(0,grid.mapFromItem(listView,0,0).y)
        var row=Math.floor((localY+grid.cellHeight/2)/grid.cellHeight)
        var first=Math.max(0,Math.min(grid.count-1,row*grid.columns))
        var letter=alphabet[0].letter
        for(var i=0;i<alphabet.length;i++)if(alphabet[i].offset<=first)letter=alphabet[i].letter;else break
        activeArtistLetter=letter
    }
    // Paging replaces the grid model; keep the scroll position across it.
    function rememberGridPosition() {
        gridContentY=listView.contentY
        restoreGridContentY=true
    }
    function retainGridPosition() {
        if(restoreGridContentY)restoreLater.restart()
    }
    function relayout() {
        updateArtistLetter()
        positionLater.restart()
    }
    Timer {id:positionLater;interval:0;onTriggered:helper.tryPositionArtist()}
    Timer {
        id:restoreLater;interval:0
        onTriggered:{
            if(!helper.restoreGridContentY || !helper.listView)return
            var listView=helper.listView
            listView.contentY=Math.max(listView.originY,Math.min(helper.gridContentY,Math.max(listView.originY,listView.contentHeight-listView.height)))
            helper.restoreGridContentY=false
        }
    }
}
